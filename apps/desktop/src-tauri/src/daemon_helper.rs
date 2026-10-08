//! Verified checkout helper in development, shared fallback for packaged apps.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const PREPARATION_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const TIMEOUT_MESSAGE: &str = "Verifying the ctld helper timed out. Wait for any ctl setup or signing operation to finish, then try again.";

#[cfg(target_os = "macos")]
pub(crate) fn register() -> io::Result<()> {
  if LOCAL_DEVELOPMENT.is_some() {
    ctl_ipc::register_preferred_contract_daemon_executable_provider(|required| {
      Box::pin(discover(required))
    })
  } else {
    ctl_ipc::register_daemon_executable_provider(|| Box::pin(discover(None)))
  }
}

#[cfg(target_os = "macos")]
include!(concat!(env!("OUT_DIR"), "/development_ctld.rs"));

#[cfg(target_os = "macos")]
async fn discover(
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> io::Result<Option<PathBuf>> {
  tokio::time::timeout(PREPARATION_TIMEOUT, async {
    loop {
      let result = select_helper(
        async {
          match &LOCAL_DEVELOPMENT {
            Some(context) => context.discover(required).await,
            None => Ok(None),
          }
        },
        async {
          match required {
            Some(required) => ctl_client::setup::discover_ctld_for_helper_contract(required).await,
            None => ctl_client::setup::discover_compatible_ctld().await,
          }
        },
      )
      .await;
      match result {
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

#[cfg(any(test, target_os = "macos"))]
async fn select_helper(
  local: impl std::future::Future<Output = Result<Option<PathBuf>, ctl_client::setup::Error>>,
  shared: impl std::future::Future<Output = Result<Option<PathBuf>, ctl_client::setup::Error>>,
) -> Result<Option<PathBuf>, ctl_client::setup::Error> {
  if let Some(executable) = local.await? {
    return Ok(Some(executable));
  }
  shared.await
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

#[cfg(test)]
mod selection_tests {
  use super::*;

  #[tokio::test]
  async fn gui_prefers_the_same_verified_checkout_helper_as_the_cli() {
    let executable = PathBuf::from("checkout/ctld.app/Contents/MacOS/ctld");
    let selected = select_helper(async { Ok(Some(executable.clone())) }, async {
      panic!("a verified checkout helper must win over shared selection")
    })
    .await
    .unwrap();
    assert_eq!(selected, Some(executable));
  }

  #[tokio::test]
  async fn missing_or_incompatible_checkout_helpers_retain_shared_discovery() {
    let executable = PathBuf::from("shared/ctld.app/Contents/MacOS/ctld");
    assert_eq!(
      select_helper(async { Ok(None) }, async { Ok(Some(executable.clone())) })
        .await
        .unwrap(),
      Some(executable)
    );
  }

  #[tokio::test]
  async fn invalid_checkout_helpers_report_verification_errors() {
    let error = select_helper(
      async {
        Err(ctl_client::setup::Error::Verification(
          "invalid signature".into(),
        ))
      },
      async { panic!("trust failures must not silently fall back") },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ctl_client::setup::Error::Verification(_)));
  }
}
