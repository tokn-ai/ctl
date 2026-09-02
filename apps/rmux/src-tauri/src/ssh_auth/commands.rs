// Tauri extracts owned command arguments from IPC.
#![allow(clippy::needless_pass_by_value)]

use super::SshPromptDto;
use crate::dto::ConnectionTargetDto;
use crate::error::CommandResult;
use serde::Deserialize;
use tauri::{WebviewWindow, ipc::Channel};

#[derive(Deserialize)]
pub struct ProbeRequest {
  target: ConnectionTargetDto,
  attempt_id: String,
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

#[tauri::command(rename_all = "snake_case")]
pub async fn probe_ssh_host(
  window: WebviewWindow,
  request: ProbeRequest,
  on_prompt: Channel<SshPromptDto>,
) -> CommandResult<()> {
  super::probe(
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
pub fn forget_ssh_credentials(request: crate::dto::TargetRequestDto) {
  super::forget(&request.target);
}
