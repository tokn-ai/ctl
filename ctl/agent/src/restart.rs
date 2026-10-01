//! A fixed, account-local maintenance operation, independent of session protocol.
use std::{io, time::Duration};

use crate::{ConnectConfig, Service};

/// Stops all ctmux sessions through the owner-only control endpoint and starts
/// the installed companion daemon. Never signals PIDs or removes live sockets.
///
/// # Errors
/// Returns an error if identity, restart support, shutdown, or startup fails.
pub async fn restart_ctmux(
  config: &ConnectConfig,
  expected_remote_id: &str,
  actual_remote_id: &str,
) -> io::Result<ctl_proto::RemoteCtmuxRestartResult> {
  if expected_remote_id.is_empty() || expected_remote_id != actual_remote_id {
    return Err(io::Error::other(
      "Remote identity changed; ctmux was not restarted.",
    ));
  }
  if config.service != Service::Ctmux {
    return Err(io::Error::other(
      "Only ctmux can be restarted by this operation.",
    ));
  }
  // Validate the installed replacement before ending any sessions.
  crate::daemon_command(config).map_err(io::Error::other)?;
  let control_path = ctmux_ipc::control_socket_path(&config.ctmux_socket)?;
  let stream = ctmux_ipc::connect_existing_daemon(&control_path)
    .await
    .map_err(|error| {
      io::Error::other(format!(
        "Cannot access ctmux restart control: {error}. Restart it manually."
      ))
    })?;
  let terminated_sessions = tokio::time::timeout(
    Duration::from_secs(15),
    ctmux_ipc::request_local_daemon_restart(stream),
  )
  .await
  .map_err(|_| io::Error::other("Restart response timed out; check the host before retrying."))?
  .map_err(io::Error::other)?;
  ctmux_ipc::wait_for_daemon_shutdown(&config.ctmux_socket, &control_path, Duration::from_secs(15))
    .await
    .map_err(io::Error::other)?;
  drop(
    crate::connect_or_start_daemon(config)
      .await
      .map_err(io::Error::other)?,
  );
  Ok(ctl_proto::RemoteCtmuxRestartResult {
    terminated_sessions,
  })
}
