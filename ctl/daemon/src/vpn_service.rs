//! Serializes VPN requests while keeping container ownership inside ctld.

use std::collections::VecDeque;
use std::future::{Future, pending};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use ctld_ipc::{VpnConnection, VpnState, VpnStatus};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::openconnect::{self, Config, ManagedVpn, Metadata};

type Reply = oneshot::Sender<Result<VpnStatus, String>>;

const MAX_QUEUED_PREPARATIONS: usize = 16;

enum Request {
  Start { source: Source, reply: Reply },
  Stop(Reply),
  Status(Reply),
}

#[derive(Clone)]
pub(super) struct VpnService {
  requests: mpsc::Sender<Request>,
}

impl VpnService {
  pub(super) async fn start(&self, env_file: PathBuf) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self
      .request(
        Request::Start {
          source: Source::EnvFile(env_file),
          reply,
        },
        result,
      )
      .await
  }

  pub(super) async fn start_connection(
    &self,
    connection: VpnConnection,
  ) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self
      .request(
        Request::Start {
          source: Source::Connection(connection),
          reply,
        },
        result,
      )
      .await
  }

  pub(super) async fn stop(&self) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self.request(Request::Stop(reply), result).await
  }

  pub(super) async fn status(&self) -> Result<VpnStatus, String> {
    let (reply, result) = oneshot::channel();
    self.request(Request::Status(reply), result).await
  }

  async fn request(
    &self,
    request: Request,
    result: oneshot::Receiver<Result<VpnStatus, String>>,
  ) -> Result<VpnStatus, String> {
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
  spawn_with(openconnect::start)
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
    self.status()
  }

  async fn exited(&mut self) -> io::Result<()> {
    self.exited().await.map(|_| ())
  }

  async fn shutdown(&mut self) {
    self.shutdown().await;
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

struct Prepared {
  identity: Identity,
  config: Config,
}

struct Preparing {
  future: Pin<Box<dyn Future<Output = Result<Prepared, String>> + Send>>,
  reply: Reply,
}

impl Source {
  async fn prepare(self) -> Result<Prepared, String> {
    match self {
      Self::EnvFile(path) => {
        let (path, config) = openconnect::read_file(path)
          .await
          .map_err(|error| error.to_string())?;
        Ok(Prepared {
          identity: Identity::EnvFile(path),
          config,
        })
      }
      Self::Connection(connection) => {
        let config = Config::from_connection(&connection).map_err(|error| error.to_string())?;
        let serialized =
          Zeroizing::new(serde_json::to_vec(&connection).map_err(|_| "invalid VPN connection")?);
        Ok(Prepared {
          identity: Identity::Connection {
            connection_id: connection.connection_id,
            fingerprint: Sha256::digest(serialized.as_slice()).into(),
          },
          config,
        })
      }
    }
  }
}

impl Identity {
  fn connection_id(&self) -> Option<String> {
    match self {
      Self::EnvFile(_) => None,
      Self::Connection { connection_id, .. } => Some(connection_id.clone()),
    }
  }

  fn status(&self, metadata: &Metadata, state: VpnState) -> VpnStatus {
    VpnStatus {
      connection_id: self.connection_id(),
      vpn_url: metadata.vpn_url.clone(),
      username: metadata.username.clone(),
      state,
      ..VpnStatus::default()
    }
  }
}

struct Active<L> {
  identity: Identity,
  metadata: Metadata,
  lease: L,
}

impl<L: Lease> Active<L> {
  fn status(&self) -> VpnStatus {
    VpnStatus {
      connection_id: self.identity.connection_id(),
      vpn_url: self.metadata.vpn_url.clone(),
      username: self.metadata.username.clone(),
      state: VpnState::Connected,
      ..self.lease.status()
    }
  }
}

struct Starting<F> {
  identity: Identity,
  metadata: Metadata,
  future: Pin<Box<F>>,
  replies: Vec<Reply>,
}

struct Stopping {
  identity: Identity,
  metadata: Metadata,
  future: Pin<Box<dyn Future<Output = ()> + Send>>,
  replies: Vec<Reply>,
}

fn begin_stop<L: Lease + 'static>(active: Active<L>, replies: Vec<Reply>) -> Stopping {
  let Active {
    identity,
    metadata,
    mut lease,
  } = active;
  Stopping {
    identity,
    metadata,
    future: Box::pin(async move {
      lease.shutdown().await;
    }),
    replies,
  }
}

fn spawn_with<L, F, S>(start: F) -> (VpnService, VpnOwner)
where
  L: Lease + 'static,
  F: Fn(Config) -> S + Send + 'static,
  S: Future<Output = io::Result<L>> + Send + 'static,
{
  let (requests, receiver) = mpsc::channel(16);
  let (shutdown, shutdown_receiver) = oneshot::channel();
  let task = tokio::spawn(serve(receiver, shutdown_receiver, start));
  (
    VpnService { requests },
    VpnOwner {
      shutdown: Some(shutdown),
      task: Some(task),
    },
  )
}

async fn serve<L, F, S>(
  mut requests: mpsc::Receiver<Request>,
  mut shutdown: oneshot::Receiver<()>,
  start: F,
) where
  L: Lease + 'static,
  F: Fn(Config) -> S,
  S: Future<Output = io::Result<L>>,
{
  let mut preparing: Option<Preparing> = None;
  let mut queued = VecDeque::new();
  let mut active: Option<Active<L>> = None;
  let mut starting: Option<Starting<S>> = None;
  let mut stopping: Option<Stopping> = None;
  loop {
    tokio::select! {
      biased;
      _ = &mut shutdown => break,
      prepared = async {
        match &mut preparing {
          Some(preparing) => (&mut preparing.future).await,
          None => pending().await,
        }
      } => {
        let preparation = preparing.take().expect("preparation completion has an owner");
        match prepared {
          Ok(prepared) => schedule_start(prepared, preparation.reply, active.as_ref(), &mut starting, stopping.is_some(), &start),
          Err(error) => { let _ = preparation.reply.send(Err(error)); }
        }
        preparing = queued.pop_front().map(begin_prepare);
      }
      started = async {
        match &mut starting {
          Some(starting) => (&mut starting.future).await,
          None => pending().await,
        }
      } => {
        let starting = starting.take().expect("startup completion has an owner");
        let result = match started {
          Ok(lease) => {
            let connection = Active { identity: starting.identity, metadata: starting.metadata, lease };
            let status = connection.status();
            active = Some(connection);
            Ok(status)
          }
          Err(error) => Err(format!("could not start VPN: {error}")),
        };
        for reply in starting.replies {
          let _ = reply.send(result.clone());
        }
      }
      () = async {
        match &mut stopping {
          Some(stopping) => (&mut stopping.future).await,
          None => pending().await,
        }
      } => {
        for reply in stopping.take().expect("shutdown completion has an owner").replies {
          let _ = reply.send(Ok(VpnStatus::default()));
        }
      }
      exited = async {
        match &mut active {
          Some(active) => active.lease.exited().await,
          None => pending().await,
        }
      } => {
        match exited {
          Ok(()) => eprintln!("OpenConnect container exited; the VPN is stopped."),
          Err(error) => eprintln!("Could not monitor OpenConnect: {error}; the VPN is stopped."),
        }
        stopping = active.take().map(|active| begin_stop(active, Vec::new()));
      }
      request = requests.recv() => {
        match request {
          Some(Request::Start { source, reply }) => {
            if stopping.is_some() {
              let _ = reply.send(Err("VPN is stopping; wait before starting it again".to_owned()));
            } else {
              enqueue_preparation(source, reply, &mut preparing, &mut queued);
            }
          }
          Some(Request::Stop(reply)) => {
            cancel_preparations(&mut preparing, &mut queued);
            cancel_start(&mut starting);
            if let Some(stopping) = &mut stopping {
              stopping.replies.push(reply);
            } else if let Some(active) = active.take() {
              stopping = Some(begin_stop(active, vec![reply]));
            } else {
              let _ = reply.send(Ok(VpnStatus::default()));
            }
          }
          Some(Request::Status(reply)) => {
            let status = current_status(active.as_ref(), starting.as_ref(), stopping.as_ref(), preparing.is_some());
            let _ = reply.send(Ok(status));
          }
          None => break,
        }
      }
    }
  }
  cancel_preparations(&mut preparing, &mut queued);
  cancel_start(&mut starting);
  shutdown_leases(active, stopping).await;
}

fn current_status<L: Lease, S>(
  active: Option<&Active<L>>,
  starting: Option<&Starting<S>>,
  stopping: Option<&Stopping>,
  preparing: bool,
) -> VpnStatus {
  if let Some(starting) = starting {
    starting
      .identity
      .status(&starting.metadata, VpnState::Starting)
  } else if let Some(stopping) = stopping {
    stopping
      .identity
      .status(&stopping.metadata, VpnState::Stopping)
  } else if let Some(active) = active {
    active.status()
  } else if preparing {
    VpnStatus {
      state: VpnState::Starting,
      ..VpnStatus::default()
    }
  } else {
    VpnStatus::default()
  }
}

async fn shutdown_leases<L: Lease>(active: Option<Active<L>>, stopping: Option<Stopping>) {
  if let Some(mut active) = active {
    active.lease.shutdown().await;
  }
  if let Some(mut stopping) = stopping {
    (&mut stopping.future).await;
    for reply in stopping.replies {
      let _ = reply.send(Ok(VpnStatus::default()));
    }
  }
}

fn schedule_start<L, F, S>(
  prepared: Prepared,
  reply: Reply,
  active: Option<&Active<L>>,
  starting: &mut Option<Starting<S>>,
  stopping: bool,
  start: &F,
) where
  L: Lease,
  F: Fn(Config) -> S,
{
  if stopping {
    let _ = reply.send(Err(
      "VPN is stopping; wait before starting it again".to_owned(),
    ));
    return;
  }
  let Prepared { identity, config } = prepared;
  if let Some(starting) = starting {
    if starting.identity == identity {
      starting.replies.push(reply);
    } else {
      let _ = reply.send(Err(
        "VPN is already starting with another configuration; stop it first".to_owned(),
      ));
    }
    return;
  }
  if let Some(active) = active {
    let result = if active.identity == identity {
      Ok(active.status())
    } else {
      Err("VPN is already running with another configuration; stop it first".to_owned())
    };
    let _ = reply.send(result);
    return;
  }
  *starting = Some(Starting {
    metadata: config.metadata.clone(),
    future: Box::pin(start(config)),
    identity,
    replies: vec![reply],
  });
}

fn begin_prepare((source, reply): (Source, Reply)) -> Preparing {
  Preparing {
    future: Box::pin(source.prepare()),
    reply,
  }
}

fn enqueue_preparation(
  source: Source,
  reply: Reply,
  preparing: &mut Option<Preparing>,
  queued: &mut VecDeque<(Source, Reply)>,
) {
  if preparing.is_none() {
    *preparing = Some(begin_prepare((source, reply)));
  } else if queued.len() < MAX_QUEUED_PREPARATIONS {
    queued.push_back((source, reply));
  } else {
    // Do not let a slow configuration read turn the bounded IPC channel into
    // an unbounded store of credential-bearing connection requests.
    let _ = reply.send(Err(
      "Too many pending VPN start requests; wait or stop the VPN before trying again".to_owned(),
    ));
  }
}

fn cancel_preparations(preparing: &mut Option<Preparing>, queued: &mut VecDeque<(Source, Reply)>) {
  if let Some(preparing) = preparing.take() {
    drop(preparing.future);
    let _ = preparing
      .reply
      .send(Err("VPN startup was cancelled".to_owned()));
  }
  for (_, reply) in queued.drain(..) {
    let _ = reply.send(Err("VPN startup was cancelled".to_owned()));
  }
}

fn cancel_start<F>(starting: &mut Option<Starting<F>>) {
  if let Some(starting) = starting.take() {
    drop(starting.future);
    for reply in starting.replies {
      let _ = reply.send(Err("VPN startup was cancelled".to_owned()));
    }
  }
}

#[cfg(test)]
mod tests;
