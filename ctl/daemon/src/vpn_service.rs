//! Serializes VPN requests while keeping container ownership inside ctld.

use std::future::{Future, pending};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use ctld_ipc::VpnStatus;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::openconnect::{self, ManagedVpn};

type Reply = oneshot::Sender<Result<VpnStatus, String>>;

enum Request {
  Start { env_file: PathBuf, reply: Reply },
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
      .request(Request::Start { env_file, reply }, result)
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
  spawn_with(|env_file| openconnect::start(openconnect::Options { env_file }))
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

struct Active<L> {
  env_file: PathBuf,
  lease: L,
}

struct Starting<F> {
  env_file: PathBuf,
  future: Pin<Box<F>>,
  reply: Reply,
}

fn spawn_with<L, F, S>(start: F) -> (VpnService, VpnOwner)
where
  L: Lease + 'static,
  F: Fn(PathBuf) -> S + Send + 'static,
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
  L: Lease,
  F: Fn(PathBuf) -> S,
  S: Future<Output = io::Result<L>>,
{
  let mut active: Option<Active<L>> = None;
  let mut starting: Option<Starting<S>> = None;
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
            let status = lease.status();
            active = Some(Active { env_file: starting.env_file, lease });
            Ok(status)
          }
          Err(error) => Err(format!("could not start VPN: {error}")),
        };
        let _ = starting.reply.send(result);
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
        stop_active(&mut active).await;
      }
      request = requests.recv() => {
        match request {
          Some(Request::Start { env_file, reply }) => {
            if starting.is_some() {
              let _ = reply.send(Err("VPN is already starting".to_owned()));
              continue;
            }
            let env_file = match env_file.canonicalize() {
              Ok(env_file) => env_file,
              Err(error) => {
                let _ = reply.send(Err(format!("could not open VPN env file: {error}")));
                continue;
              }
            };
            if let Some(active) = &active {
              let result = if active.env_file == env_file {
                Ok(active.lease.status())
              } else {
                Err("VPN is already running with another env file; stop it first".to_owned())
              };
              let _ = reply.send(result);
              continue;
            }
            starting = Some(Starting {
              future: Box::pin(start(env_file.clone())),
              env_file,
              reply,
            });
          }
          Some(Request::Stop(reply)) => {
            cancel_start(&mut starting);
            stop_active(&mut active).await;
            let _ = reply.send(Ok(VpnStatus::default()));
          }
          Some(Request::Status(reply)) => {
            let status = active.as_ref().map_or_else(VpnStatus::default, |active| active.lease.status());
            let _ = reply.send(Ok(status));
          }
          None => break,
        }
      }
    }
  }
  cancel_start(&mut starting);
  stop_active(&mut active).await;
}

fn cancel_start<F>(starting: &mut Option<Starting<F>>) {
  if let Some(starting) = starting.take() {
    // Dropping an in-progress OpenConnect start stops its heartbeat. If Docker
    // cannot observe stdin closing, the container's 15-second watchdog exits.
    drop(starting.future);
    let _ = starting
      .reply
      .send(Err("VPN startup was cancelled".to_owned()));
  }
}

async fn stop_active<L: Lease>(active: &mut Option<Active<L>>) {
  if let Some(mut active) = active.take() {
    active.lease.shutdown().await;
  }
}

#[cfg(test)]
mod tests;
