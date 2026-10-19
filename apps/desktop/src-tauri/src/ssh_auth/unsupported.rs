//! Keep non-Unix builds usable with preconfigured, noninteractive OpenSSH.
#[path = "unsupported_broker.rs"]
mod broker;
#[path = "commands.rs"]
pub mod commands;
#[path = "connection.rs"]
mod connection;
#[path = "forwarding.rs"]
mod forwarding;
#[path = "verification.rs"]
mod verification;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use ctl_client::{ConnectionTarget, SshInteraction, Transport, open_identified_ssh_service};
use serde::Serialize;
use std::future::{Ready, ready};
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
pub struct SshPromptDto {}

pub(crate) fn existing_master(
  _target: &ConnectionTargetDto,
) -> Ready<CommandResult<std::path::PathBuf>> {
  ready(Err(CommandErrorDto::new(
    "ssh_maintenance_unsupported",
    "Remote component actions require an existing SSH master on macOS or Linux.",
  )))
}

pub async fn connect(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  connection::remote_service(connect_identified(target))
    .await
    .map(|(stream, _)| stream)
}

async fn connect_identified(
  target: &ConnectionTargetDto,
) -> CommandResult<(Transport, ctl_proto::RemoteIdentity)> {
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      use_ssh_config_master: Some(true),
      ..
    }
  ) {
    return Err(CommandErrorDto::new(
      "ssh_master_selection_unsupported",
      "SSH master selection currently requires macOS or Linux.",
    ));
  }
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
    ctl_client::RemoteService::Ctmux,
  )
  .await
  .map_err(|error| CommandErrorDto::transport(&error))?;
  let identity = stream
    .remote_identity
    .as_deref()
    .expect("identified transport")
    .clone();
  target.verify_remote_identity(&identity)?;
  Ok((Transport::Ssh(stream), identity))
}

pub async fn probe(
  _app: tauri::AppHandle,
  _window: String,
  _attempt_id: String,
  target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
  _restart_check: bool,
  components_only: bool,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  if components_only {
    return Err(CommandErrorDto::new(
      "remote_component_inspection_unsupported",
      "Remote component inspection currently requires an existing SSH master on macOS or Linux.",
    ));
  }
  connection::remote_service(async {
    let (stream, identity) = connect_identified(&target).await?;
    verification::verify(stream, target.label()).await?;
    Ok(identity)
  })
  .await
}

pub fn install_agent(
  _app: tauri::AppHandle,
  _window: String,
  _attempt_id: String,
  _target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
  _on_progress: Channel<crate::dto::RemoteAgentInstallProgressDto>,
) -> Ready<CommandResult<crate::dto::RemoteAgentInstallResultDto>> {
  ready(Err(CommandErrorDto::new(
    "remote_agent_install_unsupported",
    "Remote component installation currently requires macOS or Linux.",
  )))
}

pub fn restart_ctmux(
  _app: tauri::AppHandle,
  _window: String,
  _attempt_id: String,
  _target: ConnectionTargetDto,
  _channel: Channel<SshPromptDto>,
) -> Ready<CommandResult<ctl_proto::RemoteCtmuxRestartResult>> {
  ready(Err(CommandErrorDto::new(
    "remote_restart_unsupported",
    "Remote ctmux restart currently requires macOS or Linux.",
  )))
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
pub fn forget(_target: &ConnectionTargetDto) -> Ready<CommandResult<()>> {
  ready(Ok(()))
}

pub fn disconnect(_targets: &[ConnectionTargetDto]) -> std::future::Ready<CommandResult<()>> {
  broker::unsupported()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn disabling_a_shared_master_keeps_direct_ssh_destination_validation() {
    let target = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "invalid\ndestination",
      "use_ssh_config_master": false,
    }))
    .unwrap();
    let Err(error) = connect_identified(&target).await else {
      panic!("invalid destination opened a connection")
    };
    assert_eq!(error.code, "invalid_ssh_destination");
  }

  #[tokio::test]
  async fn selecting_a_shared_master_is_rejected_before_opening_ssh() {
    let target = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "office",
      "use_ssh_config_master": true,
    }))
    .unwrap();
    let Err(error) = connect_identified(&target).await else {
      panic!("unsupported master selection opened a connection")
    };
    assert_eq!(error.code, "ssh_master_selection_unsupported");
    assert_eq!(
      error.message,
      "SSH master selection currently requires macOS or Linux.",
    );
  }
}
