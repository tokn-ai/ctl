//! Deadlines describe the operation that stalled, not an assumed authentication failure.

use std::future::Future;
use std::time::Duration;

use crate::error::{CommandErrorDto, CommandResult};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(unix)]
pub(super) enum MasterLookup {
  Existing,
  Authenticate,
}

#[cfg(unix)]
pub(super) async fn connect<M, T, F>(
  lookup: MasterLookup,
  master: impl Future<Output = CommandResult<M>>,
  open_service: impl FnOnce(M) -> F,
) -> CommandResult<T>
where
  F: Future<Output = CommandResult<T>>,
{
  connect_with_timeout(lookup, master, open_service, CONNECTION_TIMEOUT).await
}

#[cfg(unix)]
async fn connect_with_timeout<M, T, F>(
  lookup: MasterLookup,
  master: impl Future<Output = CommandResult<M>>,
  open_service: impl FnOnce(M) -> F,
  timeout: Duration,
) -> CommandResult<T>
where
  F: Future<Output = CommandResult<T>>,
{
  let master = match lookup {
    MasterLookup::Existing => tokio::time::timeout(timeout, master).await.map_err(|_| {
      CommandErrorDto::new(
        "ssh_status_timeout",
        "Timed out checking the existing SSH connection with ctld. Try again.",
      )
    })??,
    // Explicit Connect host retains its caller's authentication deadline and
    // cancellation. Credential prompts must not inherit the short status limit.
    MasterLookup::Authenticate => master.await?,
  };
  remote_service_with_timeout(open_service(master), timeout).await
}

#[cfg(not(unix))]
pub(super) async fn remote_service<T>(
  operation: impl Future<Output = CommandResult<T>>,
) -> CommandResult<T> {
  remote_service_with_timeout(operation, CONNECTION_TIMEOUT).await
}

async fn remote_service_with_timeout<T>(
  operation: impl Future<Output = CommandResult<T>>,
  timeout: Duration,
) -> CommandResult<T> {
  tokio::time::timeout(timeout, operation)
    .await
    .map_err(|_| {
      CommandErrorDto::new(
        "remote_connection_timeout",
        "Timed out opening the remote rmux service over SSH. Try again.",
      )
    })?
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::future::pending;
  #[cfg(unix)]
  use std::future::ready;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};

  struct DropFlag(Arc<AtomicBool>);

  impl Drop for DropFlag {
    fn drop(&mut self) {
      self.0.store(true, Ordering::SeqCst);
    }
  }

  #[tokio::test]
  async fn remote_service_timeout_drops_the_pending_channel_operation() {
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = DropFlag(Arc::clone(&dropped));
    let error = remote_service_with_timeout(
      async move {
        let _guard = guard;
        pending::<CommandResult<()>>().await
      },
      Duration::ZERO,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "remote_connection_timeout");
    assert!(dropped.load(Ordering::SeqCst));
    assert!(!error.message.contains("authenticate"));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn live_master_with_stalled_remote_service_is_not_an_authentication_failure() {
    let master = Arc::new(AtomicBool::new(false));
    let observed_master = Arc::clone(&master);
    let error = connect_with_timeout(
      MasterLookup::Existing,
      ready(Ok(observed_master)),
      |master| async move {
        master.store(true, Ordering::SeqCst);
        pending::<CommandResult<()>>().await
      },
      Duration::ZERO,
    )
    .await
    .unwrap_err();
    assert!(master.load(Ordering::SeqCst));
    assert_eq!(error.code, "remote_connection_timeout");
    assert!(!error.message.contains("authenticate"));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn stalled_broker_never_opens_a_remote_service() {
    let error = connect_with_timeout(
      MasterLookup::Existing,
      pending::<CommandResult<()>>(),
      |()| -> std::future::Ready<CommandResult<()>> {
        panic!("a timed-out master lookup must not open a remote channel");
      },
      Duration::ZERO,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "ssh_status_timeout");
    assert!(!error.message.contains("authenticate"));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn explicit_authentication_errors_pass_through_without_opening_a_service() {
    for lookup in [MasterLookup::Existing, MasterLookup::Authenticate] {
      let expected = CommandErrorDto::new("ssh_authentication_required", "Fixture authentication");
      let error = connect_with_timeout(
        lookup,
        ready(Err::<(), _>(expected.clone())),
        |()| -> std::future::Ready<CommandResult<()>> {
          panic!("authentication failure must not open a remote channel");
        },
        Duration::ZERO,
      )
      .await
      .unwrap_err();
      assert_eq!(error, expected);
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn successful_connection_preserves_the_selected_master_and_service() {
    for lookup in [MasterLookup::Existing, MasterLookup::Authenticate] {
      let result = connect_with_timeout(
        lookup,
        ready(Ok("fixture-master")),
        |master| ready(Ok((master, "fixture-service"))),
        Duration::ZERO,
      )
      .await
      .unwrap();
      assert_eq!(result, ("fixture-master", "fixture-service"));
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn authentication_remains_pending_until_its_caller_cancels() {
    let dropped = Arc::new(AtomicBool::new(false));
    let guard = DropFlag(Arc::clone(&dropped));
    let result = tokio::time::timeout(
      Duration::from_millis(20),
      connect_with_timeout(
        MasterLookup::Authenticate,
        async move {
          let _guard = guard;
          pending::<CommandResult<()>>().await
        },
        |()| ready(Ok(())),
        Duration::ZERO,
      ),
    )
    .await;
    assert!(
      result.is_err(),
      "the caller must own the authentication deadline"
    );
    assert!(dropped.load(Ordering::SeqCst));
  }
}
