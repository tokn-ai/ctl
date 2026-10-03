//! VPN profiles execute under the authenticated account of an SSH gateway.

use std::path::PathBuf;

use ctl_ipc::{SshTarget, VpnStatus};

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

use super::{COORDINATORS, Repository, task_error};

pub(super) fn coordinator_key(
  owner: &SshTarget,
  expected_remote_id: Option<&str>,
  connection_id: &str,
) -> CommandResult<String> {
  let mut owner = owner.clone();
  owner.normalize_master_policy();
  serde_json::to_string(&("remote_vpn", owner, expected_remote_id, connection_id))
    .map_err(CommandErrorDto::backend)
}

pub(super) async fn client_for_target(
  target: &ConnectionTargetDto,
  connection_id: &str,
) -> CommandResult<(ctl_ipc::remote_vpn::Client, String)> {
  let owner = target.to_ssh_target()?;
  let expected_remote_id = match target {
    ConnectionTargetDto::Ssh { remote_info, .. } => remote_info
      .as_ref()
      .map(|identity| identity.remote_id.clone()),
    ConnectionTargetDto::Local => None,
  };
  let key = coordinator_key(&owner, expected_remote_id.as_deref(), connection_id)?;
  let control_path = crate::ssh_auth::existing_master(target).await?;
  Ok((
    ctl_ipc::remote_vpn::Client::new(owner, expected_remote_id).with_control_path(control_path),
    key,
  ))
}

pub(super) async fn connect_saved(
  directory: PathBuf,
  connection_id: String,
  key: String,
  client: ctl_ipc::remote_vpn::Client,
) -> CommandResult<VpnStatus> {
  COORDINATORS
    .get(&key)
    .connect(|cancellation| async move {
      let load = tauri::async_runtime::spawn_blocking(move || {
        Repository::new(directory).connection(&connection_id)
      });
      let connection = tokio::select! {
        () = cancellation.wait() => return Err(super::coordinator::cancelled()),
        loaded = load => loaded.map_err(task_error)??,
      };
      cancellation.check()?;
      let status = client
        .start_connection(connection)
        .await
        .map_err(runtime_error)?;
      cancellation.check()?;
      Ok(status)
    })
    .await
}

pub(super) fn require_connected(status: &VpnStatus) -> CommandResult<()> {
  if status.state == ctl_ipc::VpnState::Connected
    && status.running
    && status.endpoint.is_some()
    && !status.status_unavailable
  {
    return Ok(());
  }
  if status.provider == ctl_ipc::VpnProvider::Tailscale
    && status.state == ctl_ipc::VpnState::Starting
    && let Some(url) = status
      .auth_url
      .as_deref()
      .filter(|url| ctl_ipc::vpn::is_tailscale_auth_url(url))
  {
    return Err(CommandErrorDto::new(
      "remote_vpn_sign_in_required",
      format!("Sign in to the VPN on the SSH gateway, then reconnect the host: {url}"),
    ));
  }
  Err(CommandErrorDto::new(
    "remote_vpn_not_connected",
    "The VPN on the SSH gateway is not connected. Check its status in the connection route, complete any device approval, then reconnect the host.",
  ))
}

#[allow(clippy::needless_pass_by_value)]
pub(super) fn runtime_error(error: ctl_ipc::remote_vpn::Error) -> CommandErrorDto {
  match error {
    ctl_ipc::remote_vpn::Error::Remote { code, message } => CommandErrorDto::new(code, message),
    ctl_ipc::remote_vpn::Error::IdentityMismatch => CommandErrorDto::new(
      "remote_identity_mismatch",
      "The SSH gateway's remote identity changed. Reconnect and verify the gateway before starting its VPN.",
    ),
    ctl_ipc::remote_vpn::Error::UnsupportedAgent
    | ctl_ipc::remote_vpn::Error::UnsupportedProtocol => CommandErrorDto::new(
      "remote_vpn_components_update_required",
      "The SSH host that runs this VPN needs updated remote components. Update that host, then retry the connection.",
    ),
    error => CommandErrorDto::new("remote_vpn_failed", error.to_string()),
  }
}

pub(super) fn route_error(
  mut error: CommandErrorDto,
  vpn_route_index: usize,
  owner_destination: Option<&str>,
) -> CommandErrorDto {
  if error.code == "remote_vpn_components_update_required"
    && let Some(owner) = owner_destination
  {
    error.message = format!(
      "The SSH host {owner} that runs this VPN needs updated remote components. Update that host, then retry the connection."
    );
    error = error.with_vpn_route_index(vpn_route_index);
  }
  error
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn remote_coordination_is_scoped_to_owner_route_and_identity() {
    let target = ConnectionTargetDto::ssh("jump-one")
      .to_ssh_target()
      .unwrap();
    let other = ConnectionTargetDto::ssh("jump-two")
      .to_ssh_target()
      .unwrap();
    let key = coordinator_key(&target, Some("account-one"), "work").unwrap();
    assert_ne!(key, "work");
    assert_eq!(
      key,
      coordinator_key(&target, Some("account-one"), "work").unwrap()
    );
    assert_ne!(
      key,
      coordinator_key(&other, Some("account-one"), "work").unwrap()
    );
    assert_ne!(
      key,
      coordinator_key(&target, Some("account-two"), "work").unwrap()
    );
    let mut explicit_default = target.clone();
    explicit_default.use_ssh_config_master = Some(false);
    assert_eq!(
      key,
      coordinator_key(&explicit_default, Some("account-one"), "work").unwrap()
    );
  }

  #[test]
  fn remote_errors_preserve_actionable_codes() {
    let error = runtime_error(ctl_ipc::remote_vpn::Error::Remote {
      code: "vpn_settings_changed".into(),
      message: "Disconnect before changing settings.".into(),
    });
    assert_eq!(error.code, "vpn_settings_changed");
    assert_eq!(error.message, "Disconnect before changing settings.");
    assert_eq!(
      runtime_error(ctl_ipc::remote_vpn::Error::IdentityMismatch).code,
      "remote_identity_mismatch"
    );
  }

  #[test]
  fn only_typed_capability_failures_request_a_component_update() {
    for error in [
      ctl_ipc::remote_vpn::Error::UnsupportedAgent,
      ctl_ipc::remote_vpn::Error::UnsupportedProtocol,
    ] {
      let error = runtime_error(error);
      assert_eq!(error.code, "remote_vpn_components_update_required");
      assert_eq!(error.vpn_route_index, None);
    }
    for error in [
      ctl_ipc::remote_vpn::Error::SshFailed,
      ctl_ipc::remote_vpn::Error::Timeout,
      ctl_ipc::remote_vpn::Error::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "SSH permission denied",
      )),
      ctl_ipc::remote_vpn::Error::UnexpectedResponse,
    ] {
      let error = runtime_error(error);
      assert_eq!(error.code, "remote_vpn_failed");
      assert_eq!(error.vpn_route_index, None);
    }
    assert_eq!(
      runtime_error(ctl_ipc::remote_vpn::Error::IdentityMismatch).code,
      "remote_identity_mismatch"
    );
  }

  #[test]
  fn repair_context_identifies_the_vpn_order_and_its_execution_owner() {
    let error = route_error(
      runtime_error(ctl_ipc::remote_vpn::Error::UnsupportedAgent),
      2,
      Some("jump-b"),
    );
    assert_eq!(error.vpn_route_index, Some(2));
    assert!(error.message.contains("SSH host jump-b"));
    let identity = runtime_error(ctl_ipc::remote_vpn::Error::IdentityMismatch);
    assert_eq!(route_error(identity.clone(), 2, Some("jump-b")), identity);
    let missing = CommandErrorDto::new("vpn_connection_not_found", "Profile missing.");
    assert_eq!(route_error(missing.clone(), 2, Some("jump-b")), missing);
    let unsupported = runtime_error(ctl_ipc::remote_vpn::Error::UnsupportedAgent);
    assert_eq!(route_error(unsupported.clone(), 0, None), unsupported);
  }

  #[test]
  fn remote_login_errors_expose_only_validated_current_tailscale_links() {
    let mut status = VpnStatus {
      provider: ctl_ipc::VpnProvider::Tailscale,
      state: ctl_ipc::VpnState::Starting,
      auth_url: Some("https://login.tailscale.com/a/testToken123".into()),
      ..VpnStatus::default()
    };
    let error = require_connected(&status).unwrap_err();
    assert_eq!(error.code, "remote_vpn_sign_in_required");
    assert!(error.message.contains(status.auth_url.as_ref().unwrap()));
    status.auth_url = Some("https://untrusted.example.test/sign-in".into());
    let error = require_connected(&status).unwrap_err();
    assert_eq!(error.code, "remote_vpn_not_connected");
    assert!(!error.message.contains("untrusted.example.test"));
  }
}
