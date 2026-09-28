//! Owns independent VPN leases and serializes changes to their registry.

use std::collections::{BTreeMap, HashSet};
use std::future::{Future, poll_fn};
use std::io;
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::task::{Context, Poll};

use ctld_ipc::{VpnConnection, VpnProvider, VpnSettings, VpnSnapshot, VpnState, VpnStatus};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::openconnect::{self, Metadata};
use crate::tailscale;

type Reply = oneshot::Sender<Result<VpnStatus, String>>;
const MAX_CONNECTIONS: usize = 16;
const MAX_WAITING_REPLIES: usize = 16;

enum Request {
  Start {
    source: Source,
    reply: Reply,
  },
  Stop {
    vpn_id: Option<String>,
    reply: Reply,
  },
  List(oneshot::Sender<Result<VpnSnapshot, String>>),
}

#[derive(Clone)]
pub(super) struct VpnService {
  requests: mpsc::Sender<Request>,
}

impl VpnService {
  pub(super) async fn start(&self, env_file: PathBuf) -> Result<VpnStatus, String> {
    self.start_source(Source::EnvFile(env_file)).await
  }

  pub(super) async fn start_connection(
    &self,
    connection: VpnConnection,
  ) -> Result<VpnStatus, String> {
    self.start_source(Source::Connection(connection)).await
  }

  async fn start_source(&self, source: Source) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self.request(Request::Start { source, reply }, result).await
  }

  pub(super) async fn stop(&self) -> Result<VpnStatus, String> {
    self.stop_selected(None).await
  }

  pub(super) async fn stop_id(&self, vpn_id: String) -> Result<VpnStatus, String> {
    self.stop_selected(Some(vpn_id)).await
  }

  async fn stop_selected(&self, vpn_id: Option<String>) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self.request(Request::Stop { vpn_id, reply }, result).await
  }

  pub(super) async fn status(&self) -> Result<VpnStatus, String> {
    Ok(
      self
        .list()
        .await?
        .connections
        .into_iter()
        .next()
        .unwrap_or_default(),
    )
  }

  pub(super) async fn list(&self) -> Result<VpnSnapshot, String> {
    let (reply, result) = oneshot::channel();
    self.request(Request::List(reply), result).await
  }

  async fn request<T>(
    &self,
    request: Request,
    result: oneshot::Receiver<Result<T, String>>,
  ) -> Result<T, String> {
    self
      .requests
      .send(request)
      .await
      .map_err(|_| unavailable())?;
    result.await.map_err(|_| unavailable())?
  }
}

pub(super) struct VpnOwner {
  shutdown: Option<oneshot::Sender<()>>,
  task: Option<JoinHandle<()>>,
}

impl VpnOwner {
  pub(super) async fn shutdown(&mut self) {
    if let Some(shutdown) = self.shutdown.take() {
      let _ = shutdown.send(());
    }
    // Keep the handle owned while awaiting: cancelling this method must still
    // allow Drop to abort the actor and release every container lease.
    if let Some(task) = &mut self.task {
      let _ = task.await;
    }
    self.task = None;
  }
}

impl Drop for VpnOwner {
  fn drop(&mut self) {
    if let Some(task) = &self.task {
      task.abort();
    }
  }
}

pub(super) fn spawn() -> (VpnService, VpnOwner) {
  spawn_with(start_provider)
}

enum Config {
  Openconnect(openconnect::Config),
  Tailscale(tailscale::Config),
}

impl Config {
  fn cancellation(&mut self) -> Option<oneshot::Sender<()>> {
    match self {
      Self::Openconnect(_) => None,
      Self::Tailscale(config) => {
        let (cancel, cancellation) = oneshot::channel();
        config.cancellation = Some(cancellation);
        Some(cancel)
      }
    }
  }
  fn metadata(&self) -> Metadata {
    match self {
      Self::Openconnect(config) => config.metadata.clone(),
      Self::Tailscale(_) => Metadata::default(),
    }
  }

  fn provider(&self) -> VpnProvider {
    match self {
      Self::Openconnect(_) => VpnProvider::Openconnect,
      Self::Tailscale(_) => VpnProvider::Tailscale,
    }
  }
}

enum ManagedVpn {
  Openconnect(openconnect::ManagedVpn),
  Tailscale(tailscale::ManagedVpn),
}

async fn start_provider(config: Config) -> io::Result<ManagedVpn> {
  match config {
    Config::Openconnect(config) => openconnect::start(config)
      .await
      .map(ManagedVpn::Openconnect),
    Config::Tailscale(config) => tailscale::start(config).await.map(ManagedVpn::Tailscale),
  }
}

fn unavailable() -> String {
  "ctld VPN service is shutting down".to_owned()
}

// The private lease boundary lets lifecycle tests exercise the actor without
// starting Docker or making a real VPN connection.
trait Lease: Send {
  fn status(&self) -> VpnStatus;
  fn exited(&mut self) -> impl Future<Output = io::Result<()>> + Send;
  fn shutdown(&mut self) -> impl Future<Output = ()> + Send;
}

impl Lease for ManagedVpn {
  fn status(&self) -> VpnStatus {
    match self {
      Self::Openconnect(lease) => lease.status(),
      Self::Tailscale(lease) => lease.status(),
    }
  }

  async fn exited(&mut self) -> io::Result<()> {
    match self {
      Self::Openconnect(lease) => lease.exited().await.map(|_| ()),
      Self::Tailscale(lease) => lease.exited().await.map(|_| ()),
    }
  }

  async fn shutdown(&mut self) {
    match self {
      Self::Openconnect(lease) => lease.shutdown().await,
      Self::Tailscale(lease) => lease.shutdown().await,
    }
  }
}

enum Source {
  EnvFile(PathBuf),
  Connection(VpnConnection),
}

#[derive(PartialEq, Eq)]
enum Identity {
  EnvFile(PathBuf),
  Connection {
    connection_id: String,
    fingerprint: [u8; 32],
  },
}

impl Identity {
  fn connection_id(&self) -> Option<String> {
    match self {
      Self::EnvFile(_) => None,
      Self::Connection { connection_id, .. } => Some(connection_id.clone()),
    }
  }
}

struct Prepared {
  identity: Identity,
  config: Config,
}

type Preparation = Pin<Box<dyn Future<Output = Result<Prepared, String>> + Send>>;
type Cleanup = Pin<Box<dyn Future<Output = ()> + Send>>;

enum Phase<L, S> {
  Preparing(Preparation),
  Starting(Pin<Box<S>>),
  Connected(L),
  Stopping(Cleanup),
}

struct Entry<L, S> {
  identity: Option<Identity>,
  env_paths: HashSet<PathBuf>,
  metadata: Metadata,
  provider: VpnProvider,
  cancellation: Option<oneshot::Sender<()>>,
  phase: Phase<L, S>,
  replies: Vec<Reply>,
}

impl<L: Lease, S> Entry<L, S> {
  fn status(&self, vpn_id: &str) -> VpnStatus {
    let mut status = match &self.phase {
      Phase::Connected(lease) => lease.status(),
      Phase::Preparing(_) | Phase::Starting(_) => VpnStatus {
        state: VpnState::Starting,
        ..VpnStatus::default()
      },
      Phase::Stopping(_) => VpnStatus {
        state: VpnState::Stopping,
        ..VpnStatus::default()
      },
    };
    status.vpn_id = Some(vpn_id.to_owned());
    status.connection_id = self.identity.as_ref().and_then(Identity::connection_id);
    status.provider = self.provider;
    if self.provider == VpnProvider::Openconnect {
      status.vpn_url.clone_from(&self.metadata.vpn_url);
      status.username.clone_from(&self.metadata.username);
    }
    status
  }

  fn join_start(&mut self, vpn_id: &str, replies: Vec<Reply>) {
    for reply in replies {
      match self.phase {
        Phase::Connected(_) => {
          let _ = reply.send(Ok(self.status(vpn_id)));
        }
        Phase::Stopping(_) => {
          let _ = reply.send(Err("VPN is stopping; wait before starting it again".into()));
        }
        _ => push_reply(&mut self.replies, reply),
      }
    }
  }
}

enum Event<L> {
  Prepared(Result<Prepared, String>),
  Started(io::Result<L>),
  Exited(io::Result<()>),
  Stopped,
}

struct Registry<L, F, S> {
  entries: BTreeMap<String, Entry<L, S>>,
  start: F,
}

impl<L, F, S> Registry<L, F, S>
where
  L: Lease + 'static,
  F: Fn(Config) -> S,
  S: Future<Output = io::Result<L>> + Send + 'static,
{
  fn new(start: F) -> Self {
    Self {
      entries: BTreeMap::new(),
      start,
    }
  }

  fn snapshot(&self) -> VpnSnapshot {
    VpnSnapshot {
      connections: self
        .entries
        .iter()
        .map(|(id, entry)| entry.status(id))
        .collect(),
      supports_multiple: true,
      ..VpnSnapshot::default()
    }
  }

  fn start(&mut self, source: Source, reply: Reply) {
    match source {
      Source::Connection(connection) => match prepare_connection(connection) {
        Ok(prepared) => self.start_prepared(prepared, reply),
        Err(error) => {
          let _ = reply.send(Err(error));
        }
      },
      Source::EnvFile(path) => {
        if let Some((id, entry)) = self
          .entries
          .iter_mut()
          .find(|(_, entry)| entry.env_paths.contains(&path))
        {
          entry.join_start(id, vec![reply]);
          return;
        }
        if self.entries.len() >= MAX_CONNECTIONS {
          let _ = reply.send(Err(
            "At most 16 VPN connections can be active or starting".into(),
          ));
          return;
        }
        let id = format!("env-{}", uuid::Uuid::new_v4().simple());
        let env_paths = HashSet::from([path.clone()]);
        let future = Box::pin(async move {
          let (path, config) = openconnect::read_file(path)
            .await
            .map_err(|error| error.to_string())?;
          Ok(Prepared {
            identity: Identity::EnvFile(path),
            config: Config::Openconnect(config),
          })
        });
        self.entries.insert(
          id,
          Entry {
            identity: None,
            env_paths,
            metadata: Metadata::default(),
            provider: VpnProvider::Openconnect,
            cancellation: None,
            phase: Phase::Preparing(future),
            replies: vec![reply],
          },
        );
      }
    }
  }

  fn start_prepared(&mut self, mut prepared: Prepared, reply: Reply) {
    let id = prepared
      .identity
      .connection_id()
      .expect("saved connection has an ID");
    if let Some(entry) = self.entries.get_mut(&id) {
      if entry.identity.as_ref() == Some(&prepared.identity) {
        entry.join_start(&id, vec![reply]);
      } else {
        let _ = reply.send(Err(
          "VPN settings changed; stop this connection before starting it again".into(),
        ));
      }
      return;
    }
    if self.entries.len() >= MAX_CONNECTIONS {
      let _ = reply.send(Err(
        "At most 16 VPN connections can be active or starting".into(),
      ));
      return;
    }
    let metadata = prepared.config.metadata();
    let provider = prepared.config.provider();
    let cancellation = prepared.config.cancellation();
    self.entries.insert(
      id,
      Entry {
        identity: Some(prepared.identity),
        env_paths: HashSet::new(),
        metadata,
        provider,
        cancellation,
        phase: Phase::Starting(Box::pin((self.start)(prepared.config))),
        replies: vec![reply],
      },
    );
  }

  fn stop(&mut self, vpn_id: Option<String>, reply: Reply) {
    let id = if let Some(id) = vpn_id {
      id
    } else {
      if self.entries.len() > 1 {
        let _ = reply.send(Err(
          "Multiple VPN connections are active; specify a VPN ID to disconnect".into(),
        ));
        return;
      }
      self.entries.keys().next().cloned().unwrap_or_default()
    };
    self.stop_entry(&id, Some(reply));
  }

  fn stop_entry(&mut self, id: &str, reply: Option<Reply>) {
    let Some(mut entry) = self.entries.remove(id) else {
      if let Some(reply) = reply {
        let _ = reply.send(Ok(VpnStatus::default()));
      }
      return;
    };
    match entry.phase {
      Phase::Connected(mut lease) => {
        entry.phase = Phase::Stopping(Box::pin(async move {
          lease.shutdown().await;
        }));
        if let Some(reply) = reply {
          push_reply(&mut entry.replies, reply);
        }
        self.entries.insert(id.to_owned(), entry);
      }
      Phase::Stopping(future) => {
        entry.phase = Phase::Stopping(future);
        if let Some(reply) = reply {
          push_reply(&mut entry.replies, reply);
        }
        self.entries.insert(id.to_owned(), entry);
      }
      Phase::Starting(future) if entry.cancellation.is_some() => {
        let _ = entry.cancellation.take().unwrap().send(());
        answer(&mut entry.replies, Err("VPN startup was cancelled".into()));
        entry.phase = Phase::Stopping(Box::pin(async move {
          if let Ok(mut lease) = future.await {
            lease.shutdown().await;
          }
        }));
        if let Some(reply) = reply {
          push_reply(&mut entry.replies, reply);
        }
        self.entries.insert(id.to_owned(), entry);
      }
      Phase::Preparing(_) | Phase::Starting(_) => {
        answer(&mut entry.replies, Err("VPN startup was cancelled".into()));
        if let Some(reply) = reply {
          let _ = reply.send(Ok(VpnStatus::default()));
        }
      }
    }
  }

  fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<(String, Event<L>)> {
    for (id, entry) in &mut self.entries {
      let event = match &mut entry.phase {
        Phase::Preparing(future) => future.as_mut().poll(cx).map(Event::Prepared),
        Phase::Starting(future) => future.as_mut().poll(cx).map(Event::Started),
        Phase::Stopping(future) => future.as_mut().poll(cx).map(|()| Event::Stopped),
        Phase::Connected(lease) => pin!(lease.exited()).poll(cx).map(Event::Exited),
      };
      if let Poll::Ready(event) = event {
        return Poll::Ready((id.clone(), event));
      }
    }
    Poll::Pending
  }

  fn event(&mut self, id: String, event: Event<L>) {
    let mut entry = self
      .entries
      .remove(&id)
      .expect("completed operation has an owner");
    match event {
      Event::Prepared(Ok(prepared)) => {
        if let Some((other_id, other)) = self
          .entries
          .iter_mut()
          .find(|(_, other)| other.identity.as_ref() == Some(&prepared.identity))
        {
          other.join_start(other_id, entry.replies);
          return;
        }
        if let Identity::EnvFile(path) = &prepared.identity {
          entry.env_paths.insert(path.clone());
        }
        entry.identity = Some(prepared.identity);
        entry.metadata = prepared.config.metadata();
        entry.provider = prepared.config.provider();
        entry.phase = Phase::Starting(Box::pin((self.start)(prepared.config)));
      }
      Event::Prepared(Err(error)) => {
        answer(&mut entry.replies, Err(error));
        return;
      }
      Event::Started(Ok(lease)) => {
        entry.cancellation = None;
        entry.phase = Phase::Connected(lease);
        let status = entry.status(&id);
        answer(&mut entry.replies, Ok(status));
      }
      Event::Started(Err(error)) => {
        answer(
          &mut entry.replies,
          Err(format!("could not start VPN: {error}")),
        );
        return;
      }
      Event::Exited(result) => {
        if result.is_err() {
          eprintln!("Could not monitor the container; cleaning up its VPN connection.");
        }
        self.entries.insert(id.clone(), entry);
        self.stop_entry(&id, None);
        return;
      }
      Event::Stopped => {
        answer(&mut entry.replies, Ok(VpnStatus::default()));
        return;
      }
    }
    self.entries.insert(id, entry);
  }

  async fn shutdown(mut self) {
    let ids: Vec<_> = self.entries.keys().cloned().collect();
    for id in ids {
      self.stop_entry(&id, None);
    }
    while !self.entries.is_empty() {
      let (id, event) = poll_fn(|cx| self.poll_event(cx)).await;
      self.event(id, event);
    }
  }
}

fn prepare_connection(connection: VpnConnection) -> Result<Prepared, String> {
  let config = match &connection.settings {
    VpnSettings::Openconnect { .. } => {
      openconnect::Config::from_connection(&connection).map(Config::Openconnect)
    }
    VpnSettings::Tailscale { .. } => {
      tailscale::Config::from_connection(&connection).map(Config::Tailscale)
    }
  }
  .map_err(|error| error.to_string())?;
  let serialized =
    Zeroizing::new(serde_json::to_vec(&connection).map_err(|_| "invalid VPN connection")?);
  let identity = Identity::Connection {
    connection_id: connection.connection_id,
    fingerprint: Sha256::digest(serialized.as_slice()).into(),
  };
  Ok(Prepared { identity, config })
}

fn answer(replies: &mut Vec<Reply>, result: Result<VpnStatus, String>) {
  if let Some(last) = replies.pop() {
    for reply in replies.drain(..) {
      let _ = reply.send(result.clone());
    }
    let _ = last.send(result);
  }
}

fn push_reply(replies: &mut Vec<Reply>, reply: Reply) {
  if replies.len() < MAX_WAITING_REPLIES {
    replies.push(reply);
  } else {
    let _ = reply.send(Err(
      "Too many pending requests for this VPN connection".into(),
    ));
  }
}

fn spawn_with<L, F, S>(start: F) -> (VpnService, VpnOwner)
where
  L: Lease + 'static,
  F: Fn(Config) -> S + Send + 'static,
  S: Future<Output = io::Result<L>> + Send + 'static,
{
  let (requests, mut receiver) = mpsc::channel(16);
  let (shutdown, mut shutdown_receiver) = oneshot::channel();
  let task = tokio::spawn(async move {
    let mut registry = Registry::new(start);
    loop {
      tokio::select! {
        biased;
        _ = &mut shutdown_receiver => break,
        (id, event) = poll_fn(|cx| registry.poll_event(cx)) => registry.event(id, event),
        request = receiver.recv() => match request {
          Some(Request::Start { source, reply }) => registry.start(source, reply),
          Some(Request::Stop { vpn_id, reply }) => registry.stop(vpn_id, reply),
          Some(Request::List(reply)) => { let _ = reply.send(Ok(registry.snapshot())); }
          None => break,
        }
      }
    }
    registry.shutdown().await;
  });
  (
    VpnService { requests },
    VpnOwner {
      shutdown: Some(shutdown),
      task: Some(task),
    },
  )
}

#[cfg(test)]
mod tests;
