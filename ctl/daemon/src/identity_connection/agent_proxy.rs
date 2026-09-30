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

use base64::Engine as _;
use sha2::{Digest, Sha256};
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
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

pub(super) async fn public_fingerprints(path: &Path) -> HashSet<String> {
  tokio::time::timeout(Duration::from_secs(2), async {
    let mut peer = connect(path).await?;
    let response = peer.request(&[REQUEST_IDENTITIES]).await.ok()?;
    let keys = parse_identities(&response)?;
    Some(
      keys
        .into_iter()
        .map(|(key, _)| {
          format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(key))
          )
        })
        .collect(),
    )
  })
  .await
  .ok()
  .flatten()
  .unwrap_or_default()
}

pub(super) struct AgentProxy {
  directory: PathBuf,
  socket: PathBuf,
  worker: JoinHandle<()>,
}

impl AgentProxy {
  pub(super) fn start(local: &Path, existing: Option<&Path>) -> io::Result<Self> {
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
    let local = local.to_owned();
    let existing = existing.map(Path::to_owned);
    let worker = tokio::spawn(async move {
      let slots = Arc::new(Semaphore::new(16));
      let mut clients = JoinSet::new();
      loop {
        tokio::select! {
          result = listener.accept() => {
            let Ok((stream, _)) = result else { break };
            let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
            let local = local.clone();
            let existing = existing.clone();
            clients.spawn(async move {
              let _permit = permit;
              let _ = serve_client(stream, &local, existing.as_deref()).await;
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
  local: &Path,
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
  if let Some(peer) = connect(local).await {
    peers.push(peer);
  }
  loop {
    let request = read_message(&mut client).await?;
    let response = match request.first().copied() {
      Some(REQUEST_IDENTITIES) if request.len() == 1 => list_identities(&mut peers, &request).await,
      Some(SIGN_REQUEST) => sign(&mut peers, &request).await,
      Some(EXTENSION) if extension_name(&request) == Some(b"session-bind@openssh.com") => {
        let mut accepted = false;
        for peer in &mut peers {
          if let Ok(response) = peer.request(&request).await {
            accepted |= response == [SUCCESS];
          }
        }
        vec![if accepted { SUCCESS } else { FAILURE }]
      }
      Some(EXTENSION) => vec![EXTENSION_FAILURE],
      // Do not forward add/remove/lock/smartcard mutations to either agent.
      _ => vec![FAILURE],
    };
    write_message(&mut client, &response).await?;
  }
}

async fn list_identities(peers: &mut [Peer], request: &[u8]) -> Vec<u8> {
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
      peer.keys.insert(key.clone());
      if seen.insert(key.clone()) && identities.len() < MAX_KEYS {
        identities.push((key, comment));
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
    vec![FAILURE]
  } else {
    response
  }
}

async fn sign(peers: &mut [Peer], request: &[u8]) -> Vec<u8> {
  let mut cursor = &request[1..];
  let Some(key) = take_string(&mut cursor) else {
    return vec![FAILURE];
  };
  if take_string(&mut cursor).is_none() || cursor.len() != 4 {
    return vec![FAILURE];
  }
  for peer in peers {
    if !peer.keys.contains(key) {
      continue;
    }
    if let Ok(response) = peer.request(request).await
      && response.first() == Some(&SIGN_RESPONSE)
    {
      return response;
    }
  }
  vec![FAILURE]
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

fn take_string<'a>(cursor: &mut &'a [u8]) -> Option<&'a [u8]> {
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
