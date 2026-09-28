//! Serializes VPN requests while keeping container ownership inside ctld.

use std::future::{Future, pending};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use ctld_ipc::{VpnConnection, VpnState, VpnStatus};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::openconnect::{self, ManagedVpn};

type Reply = oneshot::Sender<Result<VpnStatus, String>>;

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
  spawn_with(|source| async move {
    match source {
      Source::EnvFile(env_file) => openconnect::start(openconnect::Options { env_file }).await,
      Source::Connection(connection) => openconnect::start_connection(connection).await,
    }
  })
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

impl Source {
  fn identify(&mut self) -> Result<Identity, String> {
    match self {
      Self::EnvFile(path) => {
        *path = path
          .canonicalize()
          .map_err(|error| format!("could not open VPN env file: {error}"))?;
        Ok(Identity::EnvFile(path.clone()))
      }
      Self::Connection(connection) => {
        connection.validate()?;
        let serialized =
          Zeroizing::new(serde_json::to_vec(connection).map_err(|_| "invalid VPN connection")?);
        Ok(Identity::Connection {
          connection_id: connection.connection_id.clone(),
          fingerprint: Sha256::digest(serialized.as_slice()).into(),
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

  fn status(&self, state: VpnState) -> VpnStatus {
    VpnStatus {
      connection_id: self.connection_id(),
      state,
      ..VpnStatus::default()
    }
  }
}

struct Active<L> {
  identity: Identity,
  lease: L,
}

impl<L: Lease> Active<L> {
  fn status(&self) -> VpnStatus {
    VpnStatus {
      connection_id: self.identity.connection_id(),
      state: VpnState::Connected,
      ..self.lease.status()
    }
  }
}

struct Starting<F> {
  identity: Identity,
  future: Pin<Box<F>>,
  replies: Vec<Reply>,
}

struct Stopping {
  identity: Identity,
  future: Pin<Box<dyn Future<Output = ()> + Send>>,
  replies: Vec<Reply>,
}

fn begin_stop<L: Lease + 'static>(active: Active<L>, replies: Vec<Reply>) -> Stopping {
  let Active {
    identity,
    mut lease,
  } = active;
  Stopping {
    identity,
    future: Box::pin(async move {
      lease.shutdown().await;
    }),
    replies,
  }
}

fn spawn_with<L, F, S>(start: F) -> (VpnService, VpnOwner)
where
  L: Lease + 'static,
  F: Fn(Source) -> S + Send + 'static,
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
  F: Fn(Source) -> S,
  S: Future<Output = io::Result<L>>,
{
  let mut active: Option<Active<L>> = None;
  let mut starting: Option<Starting<S>> = None;
  let mut stopping: Option<Stopping> = None;
  loop {
    tokio::select! {
      biased;
      _ = &mut shutdown => break,
      started = async {
        match &mut starting {
          Some(starting) => (&mut starting.future).await,
          None => pending().await,
        }
      } => {
        let starting = starting.take().expect("startup completion has an owner");
        let result = match started {
          Ok(lease) => {
            let connection = Active { identity: starting.identity, lease };
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
            schedule_start(source, reply, active.as_ref(), &mut starting, stopping.is_some(), &start);
          }
          Some(Request::Stop(reply)) => {
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
            let status = if let Some(starting) = &starting {
              starting.identity.status(VpnState::Starting)
            } else if let Some(stopping) = &stopping {
              stopping.identity.status(VpnState::Stopping)
            } else {
              active.as_ref().map_or_else(VpnStatus::default, Active::status)
            };
            let _ = reply.send(Ok(status));
          }
          None => break,
        }
      }
    }
  }
  cancel_start(&mut starting);
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
  mut source: Source,
  reply: Reply,
  active: Option<&Active<L>>,
  starting: &mut Option<Starting<S>>,
  stopping: bool,
  start: &F,
) where
  L: Lease,
  F: Fn(Source) -> S,
{
  if stopping {
    let _ = reply.send(Err(
      "VPN is stopping; wait before starting it again".to_owned(),
    ));
    return;
  }
  let identity = match source.identify() {
    Ok(identity) => identity,
    Err(error) => {
      let _ = reply.send(Err(error));
      return;
    }
  };
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
    future: Box::pin(start(source)),
    identity,
    replies: vec![reply],
  });
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
