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
  #[serde(default)]
  components_only: bool,
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
  forward: ctl_ipc::LocalPortForward,
  enabled: bool,
}

#[derive(Deserialize)]
pub struct DisconnectSshHostRequest {
  targets: Vec<ConnectionTargetDto>,
  retained_targets: Option<Vec<ConnectionTargetDto>>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn probe_ssh_host(
  app: AppHandle,
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  super::probe(
    app,
    window.label().into(),
    request.attempt_id,
    request.target,
    on_prompt,
    request.restart_check,
    request.components_only,
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
pub async fn restart_remote_ctmux(
  app: AppHandle,
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteCtmuxRestartResult> {
  super::restart_ctmux(
    app,
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
  if let Some(retained) = request.retained_targets {
    return disconnect_removed_host(&request.targets, &retained).await;
  }
  super::disconnect(&request.targets).await
}

#[cfg(unix)]
async fn disconnect_removed_host(
  targets: &[ConnectionTargetDto],
  retained: &[ConnectionTargetDto],
) -> CommandResult<()> {
  let targets = unshared_master_targets(targets, retained);
  if targets.is_empty() {
    return Ok(());
  }
  super::disconnect(&targets).await
}

#[cfg(not(unix))]
fn disconnect_removed_host(
  _targets: &[ConnectionTargetDto],
  _retained: &[ConnectionTargetDto],
) -> std::future::Ready<CommandResult<()>> {
  // This platform has no persistent SSH broker to clean up on host removal.
  std::future::ready(Ok(()))
}

#[cfg(unix)]
fn unshared_master_targets(
  targets: &[ConnectionTargetDto],
  retained: &[ConnectionTargetDto],
) -> Vec<ConnectionTargetDto> {
  // Use the broker's normalized route identity. Display names, host IDs, and
  // gateway profile labels do not distinguish two uses of the same master.
  let mut used: std::collections::HashSet<_> = retained
    .iter()
    .filter_map(|target| target.to_ssh_target().ok())
    .collect();
  targets
    .iter()
    .filter(|target| {
      target
        .to_ssh_target()
        .is_ok_and(|target| used.insert(target))
    })
    .cloned()
    .collect()
}

#[tauri::command]
pub async fn configure_port_forward(
  request: ConfigurePortForwardRequest,
) -> CommandResult<ctl_ipc::PortForwardStatus> {
  super::broker::configure_port_forward(&request.target, request.forward, request.enabled).await
}

#[tauri::command]
pub async fn list_port_forwards(
  request: crate::dto::TargetRequestDto,
) -> CommandResult<Vec<ctl_ipc::PortForwardStatus>> {
  super::broker::list_port_forwards(&request.target).await
}

#[tauri::command]
pub async fn list_remote_listeners(
  request: crate::dto::TargetRequestDto,
) -> CommandResult<ctl_proto::TcpListenerCatalog> {
  super::broker::list_remote_listeners(&request.target).await
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[test]
  fn host_removal_keeps_shared_masters_and_deduplicates_obsolete_routes() {
    let route = |host_id: &str, user: &str, master_policy: Option<bool>| {
      serde_json::from_value::<ConnectionTargetDto>(serde_json::json!({
        "kind": "ssh",
        "host_id": host_id,
        "destination": "build.example",
        "user": user,
        "use_ssh_config_master": master_policy,
      }))
      .unwrap()
    };
    let shared = route("removed", "build", None);
    let retained = route("retained", "build", Some(false));
    let obsolete = route("removed", "other", None);
    let removed = [shared, obsolete.clone(), obsolete.clone()];
    assert_eq!(unshared_master_targets(&removed, &[retained]), [obsolete]);
  }
}
