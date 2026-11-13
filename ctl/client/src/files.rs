//! Fixed, identified file-service channels. Paths travel inside the protocol.

use std::future::ready;

use tokio::process::Command;

use crate::{CoreError, RemotePlatform, SshConnectionOptions, SshInteraction, SshTransport};

const UNIX_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/bin:$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; }; "#,
  "exec ctl-agent files",
);
const WINDOWS_COMMAND: &str = concat!(
  r#"if exist "%USERPROFILE%\.tokn\ctl\bin\ctl-agent.exe" "#,
  r#"("%USERPROFILE%\.tokn\ctl\bin\ctl-agent.exe" files) else (ctl-agent.exe files)"#,
);

/// Opens a file request channel using the selected master's existing identity.
///
/// Unix brokered requests share a bounded file pool. Other supported transports
/// invoke the fixed `files` endpoint; no requested path becomes a shell argument.
/// The caller must verify `remote_identity` before sending a file operation.
///
/// # Errors
/// Returns target validation, broker, negotiation, or SSH startup errors.
pub async fn open(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<SshTransport, CoreError> {
  crate::validate_ssh_target(destination, options)?;
  if options.remote_platform == RemotePlatform::Unix
    && let SshInteraction::Brokered {
      target,
      control_path,
    } = interaction
  {
    let stream = crate::connection::open_service(
      target.clone(),
      control_path.clone(),
      ctl_proto::transport::Service::Files,
    )
    .await?;
    return crate::broker_transport::from_stream(stream, true).await;
  }
  let mut command = Command::new(crate::SSH_PROGRAM);
  let interaction_arguments = crate::configure_ssh_interaction(&mut command, interaction);
  command
    .args(interaction_arguments)
    .args(crate::prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(match options.remote_platform {
      RemotePlatform::Unix => UNIX_COMMAND,
      RemotePlatform::Windows => WINDOWS_COMMAND,
    });
  crate::start_ssh_transport_identified(command, true, false, ready(())).await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn refuses_invalid_destinations_before_starting_ssh() {
    let result = open(
      "invalid\0host",
      &SshConnectionOptions::default(),
      &SshInteraction::Batch,
    )
    .await;
    assert!(matches!(result, Err(CoreError::InvalidSshDestination(_))));
  }

  #[test]
  fn endpoint_commands_are_fixed_for_both_remote_platforms() {
    assert!(UNIX_COMMAND.ends_with("exec ctl-agent files"));
    assert!(WINDOWS_COMMAND.ends_with("else (ctl-agent.exe files)"));
    assert!(!UNIX_COMMAND.contains("connect"));
  }
}
