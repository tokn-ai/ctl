//! Select the app's daemon and configured signed helper for VPN operations.

use std::path::PathBuf;

pub(super) fn client() -> Result<ctld_ipc::vpn::Client, ctld_ipc::vpn::VpnError> {
  Ok(ctld_ipc::vpn::Client::default().with_daemon_executable(ctld_ipc::daemon_executable()?))
}

pub(super) fn selected_daemon_executable() -> Result<Option<PathBuf>, ctld_ipc::ConnectError> {
  ctld_ipc::daemon_executable().map(Some)
}

pub(super) fn selected_socket_path() -> PathBuf {
  ctld_ipc::vpn::socket_path()
}
