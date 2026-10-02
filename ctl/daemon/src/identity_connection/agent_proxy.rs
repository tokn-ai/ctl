//! A per-attempt agent view that leaves the user's agent untouched.
//!
//! OpenSSH's session binding is connection-scoped, so each client keeps the
//! same upstream connections for binding, identity enumeration, and signing.

use std::collections::HashSet;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};

const MAX_MESSAGE: usize = 1024 * 1024;
const MAX_KEYS: usize = 1024;
const FAILURE: u8 = 5;
const SUCCESS: u8 = 6;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;
const EXTENSION: u8 = 27;
const EXTENSION_FAILURE: u8 = 28;
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_BINDINGS: usize = 16;
const MAX_BINDING_BYTES: usize = 128 * 1024;

use super::fallback::Reason;
use super::lazy::LazyIdentities;

pub(super) struct AgentProxy {
  directory: PathBuf,
  socket: PathBuf,
  worker: JoinHandle<()>,
  identities: Option<Arc<LazyIdentities>>,
}

impl AgentProxy {
  #[cfg(test)]
  pub(super) fn start(local: &Path, existing: Option<&Path>) -> io::Result<Self> {
    Self::start_with(Some(local.to_owned()), None, existing)
  }

  pub(super) fn with_identities(
    identities: LazyIdentities,
    existing: Option<&Path>,
  ) -> io::Result<Self> {
    Self::start_with(None, Some(Arc::new(identities)), existing)
  }

  fn start_with(
    local: Option<PathBuf>,
    identities: Option<Arc<LazyIdentities>>,
    existing: Option<&Path>,
  ) -> io::Result<Self> {
    let directory =
      PathBuf::from("/tmp").join(format!("ctld-agent-{}", uuid::Uuid::new_v4().simple()));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let socket = directory.join("socket");
    let listener = match UnixListener::bind(&socket) {
      Ok(listener) => listener,
      Err(error) => {
        let _ = std::fs::remove_dir(&directory);
        return Err(error);
      }
    };
    let existing = existing.map(Path::to_owned);
    let owned_identities = identities.clone();
    let worker = tokio::spawn(async move {
      let slots = Arc::new(Semaphore::new(16));
      let mut clients = JoinSet::new();
      loop {
        tokio::select! {
          result = listener.accept() => {
            let Ok((stream, _)) = result else { break };
            let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
            let local = local.clone();
            let identities = identities.clone();
            let existing = existing.clone();
            clients.spawn(async move {
              let _permit = permit;
              let _ = serve_client(stream, local.as_deref(), identities, existing.as_deref()).await;
            });
          }
          _ = clients.join_next(), if !clients.is_empty() => {}
        }
      }
    });
    Ok(Self {
      directory,
      socket,
      worker,
      identities: owned_identities,
    })
  }

  pub(super) fn socket_path(&self) -> &Path {
    &self.socket
  }

  pub(super) fn write_public_key(&self, public_key: &str, index: usize) -> io::Result<PathBuf> {
    if public_key.len() > 64 * 1024 || public_key.contains(['\n', '\r', '\0']) {
      return Err(io::Error::other("invalid public identity"));
    }
    let path = self.directory.join(format!("identity-{index}.pub"));
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&path)?;
    file.write_all(public_key.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(path)
  }
}

impl Drop for AgentProxy {
  fn drop(&mut self) {
    if let Some(identities) = &self.identities {
      identities.cancel();
    }
    self.worker.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

struct Peer {
  stream: Option<UnixStream>,
  keys: HashSet<Vec<u8>>,
}

impl Peer {
  async fn request(&mut self, request: &[u8]) -> io::Result<Vec<u8>> {
    let stream = self
      .stream
      .as_mut()
      .ok_or_else(|| io::Error::other("SSH agent is unavailable"))?;
    let result = exchange(stream, request).await;
    if result.is_err() {
      // An interrupted framed exchange cannot safely resume: its late reply
      // could otherwise be mistaken for a response to a different request.
      self.stream = None;
      self.keys.clear();
    }
    result
  }
}

async fn connect(path: &Path) -> Option<Peer> {
  let stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(path))
    .await
    .ok()?
    .ok()?;
  Some(Peer {
    stream: Some(stream),
    keys: HashSet::new(),
  })
}

async fn serve_client(
  mut client: UnixStream,
  local: Option<&Path>,
  identities: Option<Arc<LazyIdentities>>,
  existing: Option<&Path>,
) -> io::Result<()> {
  let mut peers = Vec::new();
  // Preserve the user's agent ordering; explicit IdentityFile matching in
  // OpenSSH still selects our verified public-key hints before unrelated keys.
  if let Some(path) = existing
    && let Some(peer) = connect(path).await
  {
    peers.push(peer);
  }
  if let Some(local) = local
    && let Some(peer) = connect(local).await
  {
    peers.push(peer);
  }
  let mut lazy = ClientIdentities::new(identities);
  loop {
    let request = read_message(&mut client).await?;
    let response = match request.first().copied() {
      Some(REQUEST_IDENTITIES) if request.len() == 1 => {
        list_identities(&mut peers, &mut lazy, &request).await
      }
      Some(SIGN_REQUEST) => sign(&mut peers, &mut lazy, &request).await,
      Some(EXTENSION) if extension_name(&request) == Some(b"session-bind@openssh.com") => {
        let mut accepted = false;
        for peer in &mut peers {
          if let Ok(response) = peer.request(&request).await {
            accepted |= response == [SUCCESS];
          }
        }
        accepted |= lazy.bind(&request).await;
        vec![if accepted { SUCCESS } else { FAILURE }]
      }
      Some(EXTENSION) => vec![EXTENSION_FAILURE],
      // Do not forward add/remove/lock/smartcard mutations to either agent.
      _ => vec![FAILURE],
    };
    write_message(&mut client, &response).await?;
  }
}

async fn list_identities(
  peers: &mut [Peer],
  lazy: &mut ClientIdentities,
  request: &[u8],
) -> Vec<u8> {
  let mut identities = Vec::new();
  let mut seen = HashSet::new();
  for peer in peers {
    peer.keys.clear();
    let Ok(response) = peer.request(request).await else {
      continue;
    };
    let Some(keys) = parse_identities(&response) else {
      continue;
    };
    for (key, comment) in keys {
      lazy.record_upstream(&key);
      peer.keys.insert(key.clone());
      if identities.len() < MAX_KEYS && seen.insert(key.clone()) {
        identities.push((key, comment));
      }
    }
  }
  if let Some(registry) = &lazy.registry {
    for (key, comment) in registry.public_identities() {
      if identities.len() < MAX_KEYS && seen.insert(key.to_vec()) {
        identities.push((key.to_vec(), comment.to_vec()));
      }
    }
  }
  let mut response = vec![IDENTITIES_ANSWER];
  let count = u32::try_from(identities.len()).expect("bounded identity count");
  response.extend(count.to_be_bytes());
  for (key, comment) in identities {
    push_string(&mut response, &key);
    push_string(&mut response, &comment);
  }
  if response.len() > MAX_MESSAGE {
    lazy.advertised.clear();
    vec![FAILURE]
  } else {
    lazy.advertised = seen;
    response
  }
}

async fn sign(peers: &mut [Peer], lazy: &mut ClientIdentities, request: &[u8]) -> Vec<u8> {
  let mut cursor = &request[1..];
  let Some(key) = take_string(&mut cursor) else {
    return vec![FAILURE];
  };
  if take_string(&mut cursor).is_none() || cursor.len() != 4 || !lazy.advertised.contains(key) {
    return vec![FAILURE];
  }
  for peer in peers {
    if !peer.keys.contains(key) {
      continue;
    }
    return if let Ok(response) = peer.request(request).await
      && response.first() == Some(&SIGN_RESPONSE)
    {
      response
    } else {
      // The user's agent is authoritative for its own keys. Falling through
      // could bypass its confirmation, destination, or lifetime restrictions.
      vec![FAILURE]
    };
  }
  if lazy.upstream_owned.contains(key) {
    // An upstream transport failure clears its live identity set, but does not
    // authorize a second request to bypass that agent through a saved key.
    return vec![FAILURE];
  }
  lazy
    .sign(key, request)
    .await
    .unwrap_or_else(|| vec![FAILURE])
}

struct ClientIdentities {
  registry: Option<Arc<LazyIdentities>>,
  advertised: HashSet<Vec<u8>>,
  upstream_owned: HashSet<Vec<u8>>,
  peers: std::collections::HashMap<Vec<u8>, Peer>,
  bindings: Vec<Vec<u8>>,
  binding_bytes: usize,
}

impl ClientIdentities {
  fn new(registry: Option<Arc<LazyIdentities>>) -> Self {
    Self {
      registry,
      advertised: HashSet::new(),
      upstream_owned: HashSet::new(),
      peers: std::collections::HashMap::new(),
      bindings: Vec::new(),
      binding_bytes: 0,
    }
  }

  fn record_upstream(&mut self, key: &[u8]) {
    if self.registry.is_none() || self.upstream_owned.contains(key) {
      return;
    }
    if self.upstream_owned.len() >= MAX_KEYS {
      // Do not accumulate an unbounded ownership history from a changing
      // upstream. Keeping only its native agent remains a safe fallback.
      self.registry = None;
      self.peers.clear();
      return;
    }
    self.upstream_owned.insert(key.to_vec());
  }

  async fn bind(&mut self, request: &[u8]) -> bool {
    if self.registry.is_none() {
      return false;
    }
    if !valid_binding(request)
      || self.bindings.len() >= MAX_BINDINGS
      || self.binding_bytes.saturating_add(request.len()) > MAX_BINDING_BYTES
    {
      // No later signature may omit a binding the caller attempted to add.
      self.registry = None;
      self.peers.clear();
      return false;
    }
    // An isolated agent validates the complete binding before signing. Until
    // unlocked, retain the exact bounded request without reading any secret.
    self.bindings.push(request.to_vec());
    self.binding_bytes += request.len();
    for peer in self.peers.values_mut() {
      if peer.request(request).await.ok().as_deref() != Some(&[SUCCESS]) {
        peer.stream = None;
      }
    }
    true
  }

  async fn sign(&mut self, key: &[u8], request: &[u8]) -> Option<Vec<u8>> {
    let registry = self.registry.as_ref()?;
    if registry.is_canceled() {
      return None;
    }
    if !self.peers.contains_key(key) {
      let socket = registry.agent_for(key).await?;
      let Some(mut peer) = connect(&socket).await else {
        registry.record_failure(key, Reason::AgentUnavailable);
        return None;
      };
      for binding in &self.bindings {
        if peer.request(binding).await.ok().as_deref() != Some(&[SUCCESS]) {
          registry.record_failure(key, Reason::AgentUnavailable);
          return None;
        }
      }
      let listed = peer
        .request(&[REQUEST_IDENTITIES])
        .await
        .ok()
        .and_then(|listed| parse_identities(&listed));
      if !listed.is_some_and(|listed| listed.iter().any(|(public, _)| public == key)) {
        registry.record_failure(key, Reason::AgentUnavailable);
        return None;
      }
      self.peers.insert(key.to_vec(), peer);
    }
    if registry.is_canceled() {
      return None;
    }
    let response = self.peers.get_mut(key)?.request(request).await.ok();
    if registry.is_canceled() {
      return None;
    }
    if let Some(response) = response
      && response.first() == Some(&SIGN_RESPONSE)
    {
      return Some(response);
    }
    registry.record_failure(key, Reason::AgentUnavailable);
    None
  }
}

fn valid_binding(request: &[u8]) -> bool {
  let mut cursor = &request[1..];
  take_string(&mut cursor) == Some(b"session-bind@openssh.com")
    && take_string(&mut cursor).is_some_and(|value| !value.is_empty())
    && take_string(&mut cursor).is_some_and(|value| !value.is_empty())
    && take_string(&mut cursor).is_some_and(|value| !value.is_empty())
    && matches!(cursor, [0 | 1])
}

fn extension_name(request: &[u8]) -> Option<&[u8]> {
  take_string(&mut &request[1..])
}

type AgentIdentities = Vec<(Vec<u8>, Vec<u8>)>;

fn parse_identities(response: &[u8]) -> Option<AgentIdentities> {
  if response.first() != Some(&IDENTITIES_ANSWER) {
    return None;
  }
  let mut cursor = &response[1..];
  let count = usize::try_from(take_u32(&mut cursor)?).ok()?;
  if count > MAX_KEYS {
    return None;
  }
  let mut keys = Vec::new();
  for _ in 0..count {
    keys.push((
      take_string(&mut cursor)?.to_vec(),
      take_string(&mut cursor)?.to_vec(),
    ));
  }
  cursor.is_empty().then_some(keys)
}

fn take_u32(cursor: &mut &[u8]) -> Option<u32> {
  let bytes = cursor.get(..4)?.try_into().ok()?;
  *cursor = &cursor[4..];
  Some(u32::from_be_bytes(bytes))
}

pub(super) fn take_string<'a>(cursor: &mut &'a [u8]) -> Option<&'a [u8]> {
  let count = usize::try_from(take_u32(cursor)?).ok()?;
  let value = cursor.get(..count)?;
  *cursor = &cursor[count..];
  Some(value)
}

fn push_string(output: &mut Vec<u8>, value: &[u8]) {
  output.extend(
    u32::try_from(value.len())
      .expect("bounded agent string")
      .to_be_bytes(),
  );
  output.extend(value);
}

async fn read_message(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
  let count = stream.read_u32().await? as usize;
  if count == 0 || count > MAX_MESSAGE {
    return Err(io::Error::other("invalid agent message"));
  }
  let mut bytes = vec![0; count];
  stream.read_exact(&mut bytes).await?;
  Ok(bytes)
}

async fn write_message(stream: &mut UnixStream, message: &[u8]) -> io::Result<()> {
  stream
    .write_u32(u32::try_from(message.len()).map_err(io::Error::other)?)
    .await?;
  stream.write_all(message).await
}

async fn exchange(stream: &mut UnixStream, request: &[u8]) -> io::Result<Vec<u8>> {
  tokio::time::timeout(REQUEST_TIMEOUT, async {
    write_message(stream, request).await?;
    read_message(stream).await
  })
  .await
  .map_err(|_| io::Error::other("SSH agent timed out"))?
}

#[cfg(test)]
mod tests;
