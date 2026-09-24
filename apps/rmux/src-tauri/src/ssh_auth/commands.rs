// Tauri extracts owned command arguments from IPC.
#![allow(clippy::needless_pass_by_value)]

use super::SshPromptDto;
use crate::dto::ConnectionTargetDto;
use crate::error::CommandResult;
use serde::Deserialize;
use tauri::{AppHandle, WebviewWindow, ipc::Channel};

#[derive(Deserialize)]
pub struct ProbeRequest {
  target: ConnectionTargetDto,
  attempt_id: String,
  #[serde(default)]
  restart_check: bool,
}
#[derive(Deserialize)]
pub struct ResponseRequest {
  attempt_id: String,
  prompt_id: String,
  response: Option<String>,
}
#[derive(Deserialize)]
pub struct CancelRequest {
  attempt_id: String,
}

#[derive(Deserialize)]
pub struct ConfigurePortForwardRequest {
  target: ConnectionTargetDto,
  forward: ctld_ipc::LocalPortForward,
  enabled: bool,
}

#[derive(Deserialize)]
pub struct DisconnectSshHostRequest {
  targets: Vec<ConnectionTargetDto>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn probe_ssh_host(
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  super::probe(
    window.label().into(),
    request.attempt_id,
    request.target,
    on_prompt,
    request.restart_check,
  )
  .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn install_remote_agent(
  app: AppHandle,
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
  on_progress: Channel<crate::dto::RemoteAgentInstallProgressDto>,
) -> CommandResult<crate::dto::RemoteAgentInstallResultDto> {
  super::install_agent(
    app,
    window.label().into(),
    request.attempt_id,
    request.target,
    on_prompt,
    on_progress,
  )
  .await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn restart_remote_rmux(
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteRmuxRestartResult> {
  super::restart_rmux(
    window.label().into(),
    request.attempt_id,
    request.target,
    on_prompt,
  )
  .await
}

#[tauri::command]
pub fn respond_ssh_prompt(window: WebviewWindow, request: ResponseRequest) -> CommandResult<()> {
  super::respond(
    window.label(),
    &request.attempt_id,
    &request.prompt_id,
    request.response,
  )
}

#[tauri::command]
pub fn cancel_ssh_probe(window: WebviewWindow, request: CancelRequest) {
  super::cancel(window.label(), &request.attempt_id);
}

#[tauri::command]
pub async fn forget_ssh_credentials(request: crate::dto::TargetRequestDto) -> CommandResult<()> {
  super::forget(&request.target).await
}

#[tauri::command]
pub async fn ssh_connection_status(
  request: crate::dto::TargetRequestDto,
) -> CommandResult<crate::dto::SshConnectionStatusDto> {
  super::broker::connection_status(&request.target).await
}

#[tauri::command]
pub async fn disconnect_ssh_host(request: DisconnectSshHostRequest) -> CommandResult<()> {
  super::disconnect(&request.targets).await
}

#[tauri::command]
pub async fn configure_port_forward(
  request: ConfigurePortForwardRequest,
) -> CommandResult<ctld_ipc::PortForwardStatus> {
  super::broker::configure_port_forward(&request.target, request.forward, request.enabled).await
}

#[tauri::command]
pub async fn list_port_forwards(
  request: crate::dto::TargetRequestDto,
) -> CommandResult<Vec<ctld_ipc::PortForwardStatus>> {
  super::broker::list_port_forwards(&request.target).await
}

#[tauri::command]
pub async fn list_remote_listeners(
  request: crate::dto::TargetRequestDto,
) -> CommandResult<ctl_proto::TcpListenerCatalog> {
  super::broker::list_remote_listeners(&request.target).await
}
