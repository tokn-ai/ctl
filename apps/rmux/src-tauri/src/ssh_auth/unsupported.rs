//! Keep non-Unix builds usable with preconfigured, noninteractive OpenSSH.
#[path = "commands.rs"]
pub mod commands;
#[path = "verification.rs"]
mod verification;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use ctl_core::{ConnectionTarget, SshInteraction, Transport, open_ssh_tunnel_interactive};
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
  open_ssh_tunnel_interactive(&destination, &options, &SshInteraction::Batch)
    .await
    .map(Transport::Ssh)
    .map_err(|error| CommandErrorDto::transport(&error))
}

pub async fn probe(
  _window: String,
  _attempt_id: String,
  target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
) -> CommandResult<()> {
  tokio::time::timeout(std::time::Duration::from_secs(10), async {
    verification::verify(connect(&target).await?).await
  })
  .await
  .map_err(|_| CommandErrorDto::new("ssh_timeout", "SSH connection timed out."))?
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
