//! Saved VPN connections and native adapters for the ctld-owned VPN service.

mod client;
mod coordinator;
mod enrollment;
mod models;
mod repository;
mod sign_in;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::LazyLock;

use ctl_ipc::{VpnConnection, VpnSnapshot, VpnStatus};
use tauri::Manager as _;

use crate::error::{CommandErrorDto, CommandResult};
use coordinator::{Cancellation, Coordinators};
use models::VpnSettingsInput;
pub use models::{
  BeginVpnEnrollmentRequest, ConnectVpnRequest, DeleteVpnConnectionRequest, OpenVpnSignInRequest,
  SaveVpnConnectionRequest, SaveVpnEnrollmentRequest, StopVpnRequest, VpnConnectionsSnapshot,
  VpnEnrollmentRequest, VpnEnrollmentSnapshot,
};
use repository::Repository;

/// Metadata-only saved settings for the Credentials page. The existing loader
/// removes passwords before returning this snapshot.
pub(crate) fn credential_metadata(snapshot: &VpnConnectionsSnapshot) -> Vec<CredentialMetadata> {
  snapshot
    .connections
    .iter()
    .map(|connection| {
      let settings = match &connection.settings {
        models::VpnSettingsSummary::Openconnect {
          url,
          username,
          has_password,
          ..
        } => CredentialSettings::Openconnect {
          url: url.clone(),
          username: username.clone(),
          has_password: *has_password,
        },
        models::VpnSettingsSummary::Tailscale { hostname, .. } => CredentialSettings::Tailscale {
          hostname: hostname.clone(),
        },
      };
      CredentialMetadata {
        connection_id: connection.connection_id.clone(),
        name: connection.name.clone(),
        settings,
      }
    })
    .collect()
}

pub(crate) struct CredentialMetadata {
  pub connection_id: String,
  pub name: String,
  pub settings: CredentialSettings,
}

pub(crate) enum CredentialSettings {
  Openconnect {
    url: String,
    username: String,
    has_password: bool,
  },
  Tailscale {
    hostname: Option<String>,
  },
}

// Each VPN coordinates independently across windows, including profile loading.
static COORDINATORS: LazyLock<Coordinators> = LazyLock::new(Coordinators::default);
static ENROLLMENTS: LazyLock<enrollment::Enrollments> =
  LazyLock::new(enrollment::Enrollments::default);

#[tauri::command]
pub async fn begin_vpn_enrollment(
  app: tauri::AppHandle,
  window: tauri::WebviewWindow,
  request: BeginVpnEnrollmentRequest,
) -> CommandResult<VpnEnrollmentSnapshot> {
  ENROLLMENTS.begin(directory(&app)?, window.label().into(), request)
}

#[tauri::command]
pub async fn vpn_enrollment_status(
  window: tauri::WebviewWindow,
  request: VpnEnrollmentRequest,
) -> CommandResult<VpnEnrollmentSnapshot> {
  ENROLLMENTS
    .status(window.label(), &request.enrollment_id)
    .await
}

#[tauri::command]
pub async fn save_vpn_enrollment(
  window: tauri::WebviewWindow,
  request: SaveVpnEnrollmentRequest,
) -> CommandResult<VpnConnectionsSnapshot> {
  ENROLLMENTS
    .save(
      window.label(),
      &request.enrollment_id,
      request.expected_revision,
    )
    .await
}

#[tauri::command]
pub async fn cancel_vpn_enrollment(
  window: tauri::WebviewWindow,
  request: VpnEnrollmentRequest,
) -> CommandResult<()> {
  ENROLLMENTS
    .cancel(window.label(), &request.enrollment_id)
    .await
}

pub(crate) async fn close_window(window_label: &str) {
  ENROLLMENTS.close_window(window_label).await;
}

/// The About page observes exactly the same owner selected for VPN operations.
pub(crate) fn owner_endpoint() -> (PathBuf, Result<Option<PathBuf>, ctl_ipc::ConnectError>) {
  (
    client::selected_socket_path(),
    client::selected_daemon_executable(),
  )
}

pub(crate) fn valid_connection_id(connection_id: &str) -> bool {
  !connection_id.is_empty()
    && connection_id.len() <= 128
    && !connection_id
      .chars()
      .any(|value| value.is_control() || value.is_whitespace())
}

/// Explicit host operations may start their saved VPN before authenticating SSH.
/// Dropping a host attempt stops waiting but never stops a shared VPN startup.
pub(crate) async fn ensure_for_host(
  app: &tauri::AppHandle,
  target: &crate::dto::ConnectionTargetDto,
) -> CommandResult<()> {
  let crate::dto::ConnectionTargetDto::Ssh {
    vpn_connection_id: Some(connection_id),
    ..
  } = target
  else {
    return Ok(());
  };
  if !valid_connection_id(connection_id) {
    return Err(CommandErrorDto::new(
      "invalid_vpn_route",
      "Choose a saved VPN connection in the host settings.",
    ));
  }
  let directory = directory(app)?;
  let connection_id = connection_id.clone();
  let status = await_shared_start(async move {
    connect_saved(directory, ConnectVpnRequest { connection_id }).await
  })
  .await
  .map_err(|mut error| {
    if error.code == "vpn_connection_not_found" {
      error.message =
        "The host's saved VPN no longer exists. Choose another connection in the host settings."
          .into();
    }
    error
  })?;
  require_connected(&status)
}

async fn await_shared_start<F>(start: F) -> CommandResult<VpnStatus>
where
  F: Future<Output = CommandResult<VpnStatus>> + Send + 'static,
{
  // A host does not own the VPN. Keep its operation and coordinator alive if
  // that host cancels, since another host may already be awaiting the same VPN.
  tauri::async_runtime::spawn(start)
    .await
    .map_err(task_error)?
}

fn require_connected(status: &VpnStatus) -> CommandResult<()> {
  if status.state == ctl_ipc::VpnState::Connected && status.running && status.endpoint.is_some() {
    return Ok(());
  }
  if status.state == ctl_ipc::VpnState::Starting && status.auth_url.is_some() {
    return Err(CommandErrorDto::new(
      "vpn_sign_in_required",
      "Sign in to the selected VPN on the VPN page, then reconnect the host.",
    ));
  }
  Err(CommandErrorDto::new(
    "vpn_not_connected",
    "The selected VPN is not connected. Check its settings on the VPN page and reconnect the host.",
  ))
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
  let coordinator = COORDINATORS.get(&request.connection.connection_id);
  let _change = coordinator.changes.lock().await;
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
  let coordinator = COORDINATORS.get(&request.connection_id);
  let _change = coordinator.changes.lock().await;
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
  connect_saved(directory(&app)?, request).await
}

async fn connect_saved(directory: PathBuf, request: ConnectVpnRequest) -> CommandResult<VpnStatus> {
  if !cfg!(unix) {
    return Err(runtime_error(ctl_ipc::vpn::VpnError::UnsupportedPlatform));
  }
  let coordinator = COORDINATORS.get(&request.connection_id);
  coordinator
    .connect(|cancellation| async move {
      connect_with_cancellation(
        directory,
        request,
        |connection| async move { client::client()?.start_connection(connection).await },
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
  S: Future<Output = Result<VpnStatus, ctl_ipc::vpn::VpnError>>,
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
  S: Future<Output = Result<VpnStatus, ctl_ipc::vpn::VpnError>>,
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
pub async fn vpn_status() -> CommandResult<VpnSnapshot> {
  client::client()
    .map_err(runtime_error)?
    .list()
    .await
    .map_err(runtime_error)
}

#[tauri::command]
pub async fn stop_vpn(request: StopVpnRequest) -> CommandResult<VpnStatus> {
  let coordinator = COORDINATORS.get(&request.vpn_id);
  coordinator
    .stop(|| async {
      client::client()
        .map_err(runtime_error)?
        .stop_id(&request.vpn_id)
        .await
        .map_err(runtime_error)
    })
    .await
}

#[tauri::command]
pub async fn open_vpn_sign_in(request: OpenVpnSignInRequest) -> CommandResult<()> {
  let snapshot = vpn_status().await?;
  sign_in::open(&snapshot, &request.vpn_id, sign_in::open_browser).await
}

async fn mutation_status() -> CommandResult<VpnSnapshot> {
  #[cfg(unix)]
  return vpn_status().await;
  #[cfg(not(unix))]
  Ok(VpnSnapshot::default())
}

// Owned adapter for Result::map_err.
#[allow(clippy::needless_pass_by_value)]
fn runtime_error(error: ctl_ipc::vpn::VpnError) -> CommandErrorDto {
  CommandErrorDto::new(error.code(), error.to_string())
}

fn task_error(_: tauri::Error) -> CommandErrorDto {
  CommandErrorDto::new(
    "vpn_storage_unavailable",
    "The VPN settings operation did not complete.",
  )
}
