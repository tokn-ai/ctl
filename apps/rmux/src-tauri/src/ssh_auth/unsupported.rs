//! Keep non-Unix builds usable with preconfigured, noninteractive OpenSSH.
#[path = "commands.rs"]
pub mod commands;
#[path = "verification.rs"]
mod verification;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use ctl_core::{ConnectionTarget, SshInteraction, Transport, open_identified_ssh_service};
use serde::Serialize;
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
pub struct SshPromptDto {}

/// This platform does not use the Unix askpass helper.
#[must_use]
pub fn helper_exit_code() -> Option<i32> {
  None
}

pub async fn connect(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  connect_identified(target).await.map(|(stream, _)| stream)
}

async fn connect_identified(
  target: &ConnectionTargetDto,
) -> CommandResult<(Transport, ctl_proto::RemoteIdentity)> {
  let ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select a remote SSH host.",
    ));
  };
  let stream = open_identified_ssh_service(
    &destination,
    &options,
    &SshInteraction::Batch,
    ctl_core::RemoteService::Rmux,
  )
  .await
  .map_err(|error| CommandErrorDto::transport(&error))?;
  let identity = stream
    .remote_identity
    .as_ref()
    .expect("identified transport")
    .clone();
  target.verify_remote_identity(&identity)?;
  Ok((Transport::Ssh(stream), identity))
}

pub async fn probe(
  _window: String,
  _attempt_id: String,
  target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  tokio::time::timeout(std::time::Duration::from_secs(10), async {
    let (stream, identity) = connect_identified(&target).await?;
    verification::verify(stream).await?;
    Ok(identity)
  })
  .await
  .map_err(|_| CommandErrorDto::new("ssh_timeout", "SSH connection timed out."))?
}

pub async fn install_agent(
  _app: tauri::AppHandle,
  _window: String,
  _attempt_id: String,
  _target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
  _on_progress: Channel<crate::dto::RemoteAgentInstallProgressDto>,
) -> CommandResult<crate::dto::RemoteAgentInstallResultDto> {
  Err(CommandErrorDto::new(
    "remote_agent_install_unsupported",
    "Remote component installation currently requires macOS or Linux.",
  ))
}

pub fn respond(
  _window: &str,
  _attempt_id: &str,
  _prompt_id: &str,
  _response: Option<String>,
) -> CommandResult<()> {
  Err(CommandErrorDto::new(
    "ssh_prompt_unsupported",
    "Interactive SSH prompts currently require macOS or Linux.",
  ))
}

pub fn cancel(_window: &str, _attempt_id: &str) {}
pub fn cancel_window(_window: &str) {}
pub fn forget(_target: &ConnectionTargetDto) {}
pub fn remember_configured_alias(_definition: &crate::ssh_config::SshHostDefinition) {}
