//! Passive, socket-owned registration of hosts with an active frontend.

use std::sync::{
  Arc, Mutex,
  atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use ctl_ipc::{ClientMessage, ServerMessage, SshTarget};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinHandle;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(10);

/// Keeps one active host registered independently of connection request timeouts.
/// This client never starts ctld, opens SSH, or presents authentication prompts.
#[derive(Default)]
pub struct RecoveryClient {
  watch: Mutex<Option<RecoveryWatch>>,
  generation: Arc<AtomicU64>,
}

/// A live registration worker. Dropping it closes its dedicated broker socket.
pub struct RecoveryWatch {
  target: SshTarget,
  worker: JoinHandle<()>,
}

impl RecoveryClient {
  /// Register this exact route while its frontend remains active. Repeated
  /// registration of the same target retains the existing socket and worker.
  pub fn start(&self, target: SshTarget) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
      return;
    };
    let mut watch = self
      .watch
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if watch.as_ref().is_some_and(|watch| watch.target == target) {
      return;
    }
    *watch = None;
    let generation = Arc::clone(&self.generation);
    let worker_target = target.clone();
    *watch = Some(RecoveryWatch {
      target,
      worker: runtime.spawn(async move { run(worker_target, generation).await }),
    });
  }

  /// Close the registration when the frontend releases its terminal.
  pub fn stop(&self) {
    *self
      .watch
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  }

  /// Local revision of successful authorization notifications. A daemon restart
  /// cannot reset this counter and hide a later authorization from the frontend.
  #[must_use]
  pub fn generation(&self) -> u64 {
    self.generation.load(Ordering::Acquire)
  }
}

impl Drop for RecoveryWatch {
  fn drop(&mut self) {
    self.worker.abort();
  }
}

async fn run(target: SshTarget, generation: Arc<AtomicU64>) {
  let mut retry = INITIAL_RETRY;
  loop {
    let registered = tokio::time::timeout(REGISTER_TIMEOUT, async {
      let mut stream = ctl_ipc::connect_existing().await?;
      if register(&mut stream, target.clone()).await? {
        Ok(Some(stream))
      } else {
        Ok::<_, Error>(None)
      }
    })
    .await;
    match registered {
      Ok(Ok(Some(mut stream))) => {
        retry = INITIAL_RETRY;
        let _ = read_authorizations(&mut stream, &generation).await;
      }
      // Old brokers must never receive an unknown request or trigger UI. Keep
      // retrying quietly so replacement by a newer owner can restore the lease.
      Ok(Ok(None)) => retry = MAX_RETRY,
      _ => {}
    }
    tokio::time::sleep(retry).await;
    retry = (retry * 2).min(MAX_RETRY);
  }
}

async fn register<S>(stream: &mut S, target: SshTarget) -> Result<bool, Error>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  ctl_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol: ctl_ipc::protocol_offer(),
    },
  )
  .await?;
  let Some(ServerMessage::HandshakeAccepted { protocol_version }) =
    ctl_ipc::read_frame(stream).await?
  else {
    return Err(Error::UnexpectedResponse);
  };
  if !ctl_ipc::recovery_watch_supported(protocol_version) {
    return Ok(false);
  }
  ctl_ipc::write_frame(stream, &ClientMessage::WatchRecovery { target }).await?;
  match ctl_ipc::read_frame(stream).await? {
    Some(ServerMessage::RecoveryRegistered) => Ok(true),
    _ => Err(Error::UnexpectedResponse),
  }
}

async fn read_authorizations<S>(stream: &mut S, generation: &AtomicU64) -> Result<(), Error>
where
  S: AsyncRead + Unpin,
{
  let mut last = None;
  while let Some(message) = ctl_ipc::read_frame(stream).await? {
    match message {
      ServerMessage::RecoveryAuthorized {
        generation: current,
      } => {
        if last != Some(current) {
          last = Some(current);
          generation.fetch_add(1, Ordering::AcqRel);
        }
      }
      _ => return Err(Error::UnexpectedResponse),
    }
  }
  Ok(())
}

#[derive(Debug, thiserror::Error)]
enum Error {
  #[error(transparent)]
  Connect(#[from] ctl_ipc::ConnectError),
  #[error(transparent)]
  Codec(#[from] ctl_ipc::CodecError),
  #[error("ctld returned an unexpected recovery response")]
  UnexpectedResponse,
}

#[cfg(test)]
mod tests;
