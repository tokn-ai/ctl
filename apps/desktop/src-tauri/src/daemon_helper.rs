//! Verified shared-helper fallback after the desktop's own bundled helper.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const PREPARATION_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const TIMEOUT_MESSAGE: &str = "Verifying the ctld helper timed out. Wait for any ctl setup or signing operation to finish, then try again.";

#[cfg(target_os = "macos")]
pub(crate) fn register() -> io::Result<()> {
  ctl_ipc::register_daemon_executable_provider(|| Box::pin(discover()))
}

#[cfg(target_os = "macos")]
async fn discover() -> io::Result<Option<PathBuf>> {
  tokio::time::timeout(PREPARATION_TIMEOUT, async {
    loop {
      match ctl_client::setup::discover_compatible_ctld().await {
        Ok(executable) => return Ok(executable),
        Err(ctl_client::setup::Error::Busy) => {
          // Verification shares the setup lock with the CLI. Waiting remains
          // cancellable and never installs a payload or changes the running owner.
          tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(error) => return Err(io::Error::other(error)),
      }
    }
  })
  .await
  .map_err(|_| timeout_error())?
}

pub(crate) async fn executable() -> Result<PathBuf, ctl_ipc::ConnectError> {
  // Include waiting for another caller's provider mutex in the GUI deadline.
  tokio::time::timeout(PREPARATION_TIMEOUT, ctl_ipc::prepare_daemon_executable())
    .await
    .map_err(|_| ctl_ipc::ConnectError::PrepareDaemon(timeout_error()))?
}

fn timeout_error() -> io::Error {
  io::Error::new(io::ErrorKind::TimedOut, TIMEOUT_MESSAGE)
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod tests;
