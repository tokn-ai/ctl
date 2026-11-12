//! Deadlines describe the operation that stalled, not an assumed authentication failure.

use std::future::Future;
use std::time::Duration;

use ctl_client::{
  CoreError, RemotePlatform, RemoteService, SshConnectionOptions, SshInteraction, Transport,
  open_identified_ssh_service,
};

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn require_unix_target(
  target: &ConnectionTargetDto,
  operation: &str,
) -> CommandResult<()> {
  if target.remote_platform() == RemotePlatform::Windows {
    return Err(CommandErrorDto::new(
      "windows_ssh_operation_unsupported",
      format!("{operation} is unavailable for Windows SSH hosts."),
    ));
  }
  Ok(())
}

pub(super) async fn open_terminal(
  target: &ConnectionTargetDto,
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> CommandResult<(Transport, ctl_proto::RemoteIdentity)> {
  let stream = open_identified_ssh_service(destination, options, interaction, RemoteService::Ctmux)
    .await
    .map_err(|error| terminal_error(&error, options.remote_platform))?;
  let identity = stream
    .remote_identity
    .as_deref()
    .expect("identified transport")
    .clone();
  target.verify_remote_identity(&identity)?;
  Ok((Transport::Ssh(stream), identity))
}

fn terminal_error(error: &CoreError, platform: RemotePlatform) -> CommandErrorDto {
  let diagnostic = CommandErrorDto::transport(error);
  if platform == RemotePlatform::Windows
    && (matches!(
      diagnostic.code.as_str(),
      "ctl_agent_not_found" | "ctl_agent_identity_unsupported"
    ) || matches!(error, CoreError::SshStartup(message)
      if message.contains("'ctl-agent.exe' is not recognized")))
  {
    return CommandErrorDto::new(
      "windows_remote_agent_setup_required",
      "Install the Windows components with `ctlup update --destination <ssh-alias> --remote-platform windows`, then reconnect. Use the default cmd.exe SSH shell.",
    );
  }
  diagnostic
}

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
        "Timed out opening the remote ctmux service over SSH. Try again.",
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

  #[test]
  fn missing_windows_agent_suggests_ctlup_without_masking_ssh_errors() {
    for error in [
      CoreError::AgentNotFound,
      CoreError::IdentityUnsupported,
      CoreError::SshStartup(
        "'ctl-agent.exe' is not recognized as an internal or external command".into(),
      ),
    ] {
      let error = terminal_error(&error, RemotePlatform::Windows);
      assert_eq!(error.code, "windows_remote_agent_setup_required");
      assert!(error.message.contains("ctlup update"));
      assert!(error.message.contains("--remote-platform windows"));
    }
    assert_eq!(
      terminal_error(&CoreError::AgentNotFound, RemotePlatform::Unix).code,
      "ctl_agent_not_found"
    );
    let denied = CoreError::SshStartup("Permission denied (publickey).".into());
    assert_eq!(
      terminal_error(&denied, RemotePlatform::Windows).code,
      "ssh_authentication_failed"
    );
    let changed = CoreError::SshStartup("REMOTE HOST IDENTIFICATION HAS CHANGED".into());
    assert_eq!(
      terminal_error(&changed, RemotePlatform::Windows).code,
      "ssh_host_key_failed"
    );
  }

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
