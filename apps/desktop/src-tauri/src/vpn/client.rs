//! Select the app's daemon endpoint without preparing a helper for passive use.

use std::path::PathBuf;

pub(super) fn client() -> ctl_ipc::vpn::Client {
  ctl_ipc::vpn::Client::default()
}

pub(super) fn selected_socket_path() -> PathBuf {
  ctl_ipc::vpn::socket_path()
}
