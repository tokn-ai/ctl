//! Saved VPN connections and native adapters for the ctld-owned VPN service.

mod coordinator;
mod repository;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;

use ctld_ipc::{VpnConnection, VpnStatus};
use serde::{Deserialize, Serialize};
use tauri::Manager as _;
use zeroize::Zeroizing;

use crate::error::{CommandErrorDto, CommandResult};
use coordinator::{Cancellation, Coordinator};
use repository::Repository;

// A delete must not race a connect from another window in this app instance.
static COORDINATOR: Coordinator = Coordinator::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VpnConnectionSummary {
  connection_id: String,
  name: String,
  url: String,
  username: String,
  has_password: bool,
  auth_method: Option<String>,
  target_ip: Option<String>,
}

impl From<&VpnConnection> for VpnConnectionSummary {
  fn from(connection: &VpnConnection) -> Self {
    Self {
      connection_id: connection.connection_id.clone(),
      name: connection.name.clone(),
      url: connection.url.clone(),
      username: connection.username.clone(),
      has_password: !connection.password.is_empty(),
      auth_method: connection.auth_method.clone(),
      target_ip: connection.target_ip.clone(),
    }
  }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VpnConnectionsSnapshot {
  revision: Option<String>,
  connections: Vec<VpnConnectionSummary>,
}

// Password-bearing types deliberately do not implement Debug or Serialize.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VpnConnectionInput {
  connection_id: String,
  name: String,
  url: String,
  username: String,
  password: Option<Zeroizing<String>>,
  auth_method: Option<String>,
  target_ip: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveVpnConnectionRequest {
  expected_revision: Option<String>,
  connection: VpnConnectionInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteVpnConnectionRequest {
  expected_revision: Option<String>,
  connection_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectVpnRequest {
  connection_id: String,
}

fn directory(app: &tauri::AppHandle) -> CommandResult<PathBuf> {
  app
    .path()
    .app_config_dir()
    .map_err(|_| CommandErrorDto::new("vpn_storage_unavailable", "Could not locate VPN settings."))
}

#[tauri::command]
pub async fn load_vpn_connections(app: tauri::AppHandle) -> CommandResult<VpnConnectionsSnapshot> {
  let directory = directory(&app)?;
  tauri::async_runtime::spawn_blocking(move || Repository::new(directory).load())
    .await
    .map_err(task_error)?
}

#[tauri::command]
pub async fn save_vpn_connection(
  app: tauri::AppHandle,
  request: SaveVpnConnectionRequest,
) -> CommandResult<VpnConnectionsSnapshot> {
  let directory = directory(&app)?;
  let _change = COORDINATOR.changes.lock().await;
  let status = mutation_status().await?;
  tauri::async_runtime::spawn_blocking(move || {
    Repository::new(directory).save_with_status(request, &status)
  })
  .await
  .map_err(task_error)?
}

#[tauri::command]
pub async fn delete_vpn_connection(
  app: tauri::AppHandle,
  request: DeleteVpnConnectionRequest,
) -> CommandResult<VpnConnectionsSnapshot> {
  let directory = directory(&app)?;
  let _change = COORDINATOR.changes.lock().await;
  let status = mutation_status().await?;
  tauri::async_runtime::spawn_blocking(move || Repository::new(directory).delete(&request, &status))
    .await
    .map_err(task_error)?
}

#[tauri::command]
pub async fn connect_vpn(
  app: tauri::AppHandle,
  request: ConnectVpnRequest,
) -> CommandResult<VpnStatus> {
  if !cfg!(unix) {
    return Err(runtime_error(ctld_ipc::vpn::VpnError::UnsupportedPlatform));
  }
  COORDINATOR
    .connect(|cancellation| async move {
      connect_with_cancellation(
        directory(&app)?,
        request,
        ctld_ipc::vpn::start_connection,
        Some(cancellation),
      )
      .await
    })
    .await
}

#[cfg(test)]
async fn connect_with<F, S>(
  directory: PathBuf,
  request: ConnectVpnRequest,
  start: F,
) -> CommandResult<VpnStatus>
where
  F: FnOnce(VpnConnection) -> S,
  S: Future<Output = Result<VpnStatus, ctld_ipc::vpn::VpnError>>,
{
  connect_with_cancellation(directory, request, start, None).await
}

async fn connect_with_cancellation<F, S>(
  directory: PathBuf,
  request: ConnectVpnRequest,
  start: F,
  cancellation: Option<Cancellation<'_>>,
) -> CommandResult<VpnStatus>
where
  F: FnOnce(VpnConnection) -> S,
  S: Future<Output = Result<VpnStatus, ctld_ipc::vpn::VpnError>>,
{
  let load = tauri::async_runtime::spawn_blocking(move || {
    Repository::new(directory).connection(&request.connection_id)
  });
  let connection = if let Some(cancellation) = cancellation {
    tokio::select! {
      () = cancellation.wait() => return Err(coordinator::cancelled()),
      loaded = load => loaded.map_err(task_error)??,
    }
  } else {
    load.await.map_err(task_error)??
  };
  if let Some(cancellation) = cancellation {
    cancellation.check()?;
  }
  let result = start(connection).await.map_err(runtime_error);
  if let Some(cancellation) = cancellation {
    cancellation.check()?;
  }
  result
}

#[tauri::command]
pub async fn vpn_status() -> CommandResult<VpnStatus> {
  ctld_ipc::vpn::status().await.map_err(runtime_error)
}

#[tauri::command]
pub async fn stop_vpn() -> CommandResult<VpnStatus> {
  COORDINATOR
    .stop(|| async { ctld_ipc::vpn::stop().await.map_err(runtime_error) })
    .await
}

async fn mutation_status() -> CommandResult<VpnStatus> {
  #[cfg(unix)]
  return vpn_status().await;
  #[cfg(not(unix))]
  Ok(VpnStatus::default())
}

// Owned adapter for Result::map_err.
#[allow(clippy::needless_pass_by_value)]
fn runtime_error(error: ctld_ipc::vpn::VpnError) -> CommandErrorDto {
  CommandErrorDto::new(error.code(), error.to_string())
}

fn task_error(_: tauri::Error) -> CommandErrorDto {
  CommandErrorDto::new(
    "vpn_storage_unavailable",
    "The VPN settings operation did not complete.",
  )
}
