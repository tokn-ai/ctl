//! Shared ctld resolution with a GUI deadline; policy lives in ctl-client.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use ctl_ipc::resolver::{Intent, Request, Selection};

pub(crate) const PREPARATION_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const TIMEOUT_MESSAGE: &str = "Verifying the ctld helper timed out. Wait for any ctl setup or signing operation to finish, then try again.";

pub(crate) fn register() -> io::Result<()> {
  ctl_ipc::resolver::register(|request| {
    Box::pin(async move {
      tokio::time::timeout(
        PREPARATION_TIMEOUT,
        ctl_client::ctld::Resolver::desktop(LOCAL_DEVELOPMENT.as_ref()).resolve(request),
      )
      .await
      .map_err(|_| timeout_error())?
    })
  })
}

include!(concat!(env!("OUT_DIR"), "/development_ctld.rs"));

pub(crate) async fn inspect() -> Result<Selection, ctl_ipc::ConnectError> {
  resolve(Intent::Inspect).await
}

pub(crate) async fn executable() -> Result<PathBuf, ctl_ipc::ConnectError> {
  resolve(Intent::Prepare)
    .await?
    .ready_executable()
    .map_err(ctl_ipc::ConnectError::PrepareDaemon)
}

async fn resolve(intent: Intent) -> Result<Selection, ctl_ipc::ConnectError> {
  // Include waiting for another caller's provider mutex in the GUI deadline.
  tokio::time::timeout(
    PREPARATION_TIMEOUT,
    ctl_ipc::resolver::resolve(Request::new(intent)),
  )
  .await
  .map_err(|_| ctl_ipc::ConnectError::PrepareDaemon(timeout_error()))?
}

fn timeout_error() -> io::Error {
  io::Error::new(io::ErrorKind::TimedOut, TIMEOUT_MESSAGE)
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod tests;
