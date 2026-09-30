//! Keep VPN ownership stable when signed development isolates the SSH helper.

use std::path::PathBuf;

pub(super) fn client() -> Result<ctld_ipc::vpn::Client, ctld_ipc::vpn::VpnError> {
  let client = ctld_ipc::vpn::Client::new(selected_socket_path());
  if let Some(executable) = selected_daemon_executable()? {
    // The launcher's signed bundle is deleted on exit. A shared owner must use
    // the durable helper beside the app, including for its future child tasks.
    Ok(client.with_daemon_executable(executable))
  } else {
    Ok(client)
  }
}

pub(super) fn selected_daemon_executable() -> Result<Option<PathBuf>, ctld_ipc::ConnectError> {
  if std::env::var_os("RMUX_DEV_DAEMON_SUPERVISOR").is_some() {
    ctld_ipc::default_daemon_executable().map(Some)
  } else {
    Ok(None)
  }
}

pub(super) fn selected_socket_path() -> PathBuf {
  socket_path(
    std::env::var_os("CTLD_VPN_SOCKET_PATH").map(PathBuf::from),
    std::env::var_os("RMUX_DEV_DAEMON_SUPERVISOR").is_some(),
    ctld_ipc::socket_path(),
    ctld_ipc::default_socket_path(),
  )
}

fn socket_path(
  vpn_override: Option<PathBuf>,
  signed_development: bool,
  inherited: PathBuf,
  shared: PathBuf,
) -> PathBuf {
  vpn_override.unwrap_or({
    if signed_development {
      // The signed launcher replaces CTLD_SOCKET_PATH for its temporary SSH
      // helper. VPNs must remain visible to the CLI and survive that launcher.
      shared
    } else {
      inherited
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn signed_development_uses_the_shared_vpn_owner() {
    assert_eq!(
      socket_path(
        None,
        true,
        "temporary-helper.sock".into(),
        "shared-owner.sock".into()
      ),
      PathBuf::from("shared-owner.sock")
    );
  }

  #[test]
  fn ordinary_launches_preserve_daemon_socket_selection() {
    assert_eq!(
      socket_path(
        None,
        false,
        "configured-owner.sock".into(),
        "shared-owner.sock".into()
      ),
      PathBuf::from("configured-owner.sock")
    );
  }

  #[test]
  fn explicit_vpn_socket_overrides_both_launch_modes() {
    let selected = PathBuf::from("custom-vpn.sock");
    for signed_development in [false, true] {
      assert_eq!(
        socket_path(
          Some(selected.clone()),
          signed_development,
          "ssh-helper.sock".into(),
          "shared-owner.sock".into()
        ),
        selected
      );
    }
  }
}
