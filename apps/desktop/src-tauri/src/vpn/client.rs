//! Select the app's daemon endpoint without preparing a helper for passive use.

use std::path::PathBuf;

pub(super) fn client() -> ctl_ipc::vpn::Client {
  ctl_ipc::vpn::Client::default()
}

pub(super) fn selected_daemon_executable() -> Result<Option<PathBuf>, ctl_ipc::ConnectError> {
  ctl_ipc::daemon_executable().map(Some)
}

pub(super) fn selected_socket_path() -> PathBuf {
  ctl_ipc::vpn::socket_path()
}
