// Tauri extracts the owned request from IPC.
#![allow(clippy::needless_pass_by_value)]

use crate::dto::TargetRequestDto;
use crate::error::CommandResult;

/// Observe an SSH greeting without authentication, session creation, or VPN startup.
#[tauri::command]
pub async fn ssh_reachability(
  request: TargetRequestDto,
) -> CommandResult<ctl_core::ssh_reachability::SshReachability> {
  let target = request.target.to_ssh_target()?;
  Ok(ctl_core::ssh_reachability::probe(&target).await)
}
