//! Select the app's daemon and configured signed helper for VPN operations.

use std::path::PathBuf;

pub(super) fn client() -> Result<ctl_ipc::vpn::Client, ctl_ipc::vpn::VpnError> {
  Ok(ctl_ipc::vpn::Client::default().with_daemon_executable(ctl_ipc::daemon_executable()?))
}

pub(super) fn selected_daemon_executable() -> Result<Option<PathBuf>, ctl_ipc::ConnectError> {
  ctl_ipc::daemon_executable().map(Some)
}

pub(super) fn selected_socket_path() -> PathBuf {
  ctl_ipc::vpn::socket_path()
}
