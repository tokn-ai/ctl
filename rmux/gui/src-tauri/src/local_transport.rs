use crate::error::CommandErrorDto;

#[cfg(unix)]
use rmux_ipc::{LocalControlClientError, LocalControlErrorCode};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;
#[cfg(unix)]
use tokio::time::timeout;

/// Details returned after a local daemon has been replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartLocalDaemonOutcome {
  pub terminated_sessions: u32,
}

/// Result of a non-destructive local restart capability probe.
///
/// A current GUI must not detach its active session merely to discover that a
/// legacy daemon lacks the owner-only local-control endpoint.
#[derive(Debug)]
pub(crate) enum RestartDaemonPreflight {
  Supported(ArmedRestart),
  DaemonAbsent(RestartDaemonPaths),
}

impl RestartDaemonPreflight {
  pub(crate) fn requires_attachment_detach(&self) -> bool {
    matches!(self, Self::Supported(_))
  }
}

#[derive(Debug)]
pub(crate) struct RestartDaemonPaths {
  data_socket_path: PathBuf,
  control_socket_path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ArmedRestart {
  paths: RestartDaemonPaths,
  stream: LocalStream,
}

#[cfg(unix)]
const DAEMON_RESTART_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
// A cooperative restart ends all sessions and waits for live data/control
// connections to drain naturally. Keep the operation bounded without forcing
// either endpoint closed.
#[cfg(unix)]
const DAEMON_RESTART_DRAIN_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(unix)]
pub type LocalStream = tokio::net::UnixStream;

#[cfg(not(unix))]
pub type LocalStream = tokio::io::DuplexStream;

#[cfg(unix)]
pub async fn connect() -> Result<LocalStream, CommandErrorDto> {
  rmux_ipc::connect_or_start_daemon(&rmux_ipc::socket_path())
    .await
    .map_err(CommandErrorDto::backend)
}

/// Verifies whether the daemon currently serving the data endpoint exposes
/// the separate owner-only local-control endpoint.
///
/// This probe has no side effects. In particular, a running legacy daemon
/// keeps the GUI attachment intact and produces `daemon_restart_unsupported`
/// rather than receiving an unknown raw `rmux-proto` message.
///
/// # Errors
///
/// Returns a typed unsupported error when data is live but the paired control
/// endpoint is absent or does not advertise cooperative restart.
#[cfg(unix)]
pub async fn preflight_restart_daemon() -> Result<RestartDaemonPreflight, CommandErrorDto> {
  let data_socket_path = rmux_ipc::socket_path();
  let control_socket_path =
    rmux_ipc::control_socket_path(&data_socket_path).map_err(restart_preflight_failure)?;
  preflight_restart_daemon_at(&data_socket_path, &control_socket_path).await
}

#[cfg(unix)]
async fn preflight_restart_daemon_at(
  data_socket_path: &Path,
  control_socket_path: &Path,
) -> Result<RestartDaemonPreflight, CommandErrorDto> {
  match rmux_ipc::connect_existing_daemon(control_socket_path).await {
    Ok(mut stream) => {
      let capabilities = preflight_local_control_handshake_with_timeout(&mut stream).await?;
      if capabilities.restart_supported {
        Ok(RestartDaemonPreflight::Supported(ArmedRestart {
          paths: RestartDaemonPaths {
            data_socket_path: data_socket_path.to_path_buf(),
            control_socket_path: control_socket_path.to_path_buf(),
          },
          stream,
        }))
      } else {
        Err(restart_unsupported(
          "the running rmuxd does not advertise cooperative restart support",
        ))
      }
    }
    Err(error) if error.is_endpoint_unavailable() => {
      match rmux_ipc::connect_existing_daemon(data_socket_path).await {
        Ok(stream) => {
          drop(stream);
          Err(restart_unsupported(
            "the running rmuxd has no owner-only local-control endpoint; restart it manually to upgrade",
          ))
        }
        Err(data_error) if data_error.is_endpoint_unavailable() => {
          Ok(RestartDaemonPreflight::DaemonAbsent(RestartDaemonPaths {
            data_socket_path: data_socket_path.to_path_buf(),
            control_socket_path: control_socket_path.to_path_buf(),
          }))
        }
        Err(data_error) => Err(restart_preflight_failure(data_error)),
      }
    }
    Err(error) => Err(restart_preflight_failure(error)),
  }
}

/// Gracefully replaces the local daemon through its owner-only control
/// endpoint, without PID signals, live-socket unlinking, or a client-side
/// list-and-kill race.
///
/// Call [`preflight_restart_daemon`] before detaching a live GUI attachment,
/// then pass the returned armed preflight here. A supported control stream is
/// retained across that transition, so restart never re-probes a potentially
/// different endpoint after the view has detached.
///
/// # Errors
///
/// Returns an error when cooperative restart is unsupported, the current
/// daemon fails to drain naturally, or a fresh daemon cannot be started and
/// health-checked.
#[cfg(unix)]
pub(crate) async fn restart_daemon(
  preflight: RestartDaemonPreflight,
) -> Result<RestartLocalDaemonOutcome, CommandErrorDto> {
  let (paths, terminated_sessions) = match preflight {
    RestartDaemonPreflight::Supported(mut armed) => {
      let terminated_sessions = request_restart_with_timeout(&mut armed.stream)
        .await
        .map_err(restart_transition_error)?;
      drop(armed.stream);
      (armed.paths, terminated_sessions)
    }
    RestartDaemonPreflight::DaemonAbsent(paths) => (paths, 0),
  };

  rmux_ipc::wait_for_daemon_shutdown(
    &paths.data_socket_path,
    &paths.control_socket_path,
    DAEMON_RESTART_DRAIN_TIMEOUT,
  )
  .await
  .map_err(|error| {
    CommandErrorDto::new(
      "daemon_restart_drain_failed",
      format!("rmuxd did not stop cleanly: {error}. It was not forcibly restarted."),
    )
  })?;

  let data_stream = rmux_ipc::connect_or_start_daemon(&paths.data_socket_path)
    .await
    .map_err(CommandErrorDto::backend)?;
  drop(data_stream);
  let mut control_stream = rmux_ipc::connect_existing_daemon(&paths.control_socket_path)
    .await
    .map_err(CommandErrorDto::backend)?;
  let capabilities = local_control_handshake_with_timeout(&mut control_stream)
    .await
    .map_err(restart_transition_error)?;
  if !capabilities.restart_supported {
    return Err(restart_transition_error(restart_unsupported(
      "the replacement rmuxd does not advertise cooperative restart support",
    )));
  }

  Ok(RestartLocalDaemonOutcome {
    terminated_sessions,
  })
}

#[cfg(unix)]
async fn local_control_handshake_with_timeout(
  stream: &mut LocalStream,
) -> Result<rmux_ipc::LocalControlCapabilities, CommandErrorDto> {
  timeout(
    DAEMON_RESTART_REQUEST_TIMEOUT,
    rmux_ipc::local_control_handshake(stream),
  )
  .await
  .map_err(|_elapsed| {
    CommandErrorDto::new(
      "daemon_restart_request_timeout",
      "rmuxd did not respond to the local-control handshake within three seconds",
    )
  })?
  .map_err(local_control_error)
}

#[cfg(unix)]
async fn preflight_local_control_handshake_with_timeout(
  stream: &mut LocalStream,
) -> Result<rmux_ipc::LocalControlCapabilities, CommandErrorDto> {
  timeout(
    DAEMON_RESTART_REQUEST_TIMEOUT,
    rmux_ipc::local_control_handshake(stream),
  )
  .await
  .map_err(|_elapsed| {
    restart_unsupported(
      "the running rmuxd did not complete the owner-only local-control handshake; restart it manually to upgrade",
    )
  })?
  .map_err(preflight_local_control_error)
}

#[cfg(unix)]
async fn request_restart_with_timeout(stream: &mut LocalStream) -> Result<u32, CommandErrorDto> {
  timeout(
    DAEMON_RESTART_REQUEST_TIMEOUT,
    rmux_ipc::request_local_daemon_restart_after_handshake(stream),
  )
  .await
  .map_err(|_elapsed| {
    CommandErrorDto::new(
      "daemon_restart_request_timeout",
      "rmuxd did not respond to the cooperative restart request within three seconds",
    )
  })?
  .map_err(local_control_error)
}

#[cfg(unix)]
fn local_control_error(error: LocalControlClientError) -> CommandErrorDto {
  match error {
    LocalControlClientError::RestartUnsupported => restart_unsupported(
      "the running rmuxd does not support cooperative restart; restart it manually to upgrade",
    ),
    LocalControlClientError::Server {
      code: LocalControlErrorCode::RestartUnsupported,
      message,
    } => restart_unsupported(message),
    LocalControlClientError::Server {
      code: LocalControlErrorCode::RestartInProgress,
      message,
    } => CommandErrorDto::new("daemon_restart_in_progress", message),
    LocalControlClientError::Server { code, message } => {
      CommandErrorDto::new(local_control_error_code(code), message)
    }
    error => CommandErrorDto::backend(error),
  }
}

#[cfg(unix)]
fn local_control_error_code(code: LocalControlErrorCode) -> &'static str {
  match code {
    LocalControlErrorCode::InvalidRequest => "local_control_invalid_request",
    LocalControlErrorCode::ProtocolVersionMismatch => "local_control_protocol_version_mismatch",
    LocalControlErrorCode::RestartUnsupported => "daemon_restart_unsupported",
    LocalControlErrorCode::RestartInProgress => "daemon_restart_in_progress",
    LocalControlErrorCode::Internal => "local_control_internal",
  }
}

#[cfg(unix)]
fn restart_unsupported(message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new("daemon_restart_unsupported", message)
}

#[cfg(unix)]
fn preflight_local_control_error(error: LocalControlClientError) -> CommandErrorDto {
  restart_preflight_failure(error)
}

#[cfg(unix)]
fn restart_preflight_failure(error: impl std::fmt::Display) -> CommandErrorDto {
  restart_unsupported(format!(
    "the running rmuxd cannot safely perform cooperative restart: {error}"
  ))
}

#[cfg(unix)]
fn restart_transition_error(error: CommandErrorDto) -> CommandErrorDto {
  let CommandErrorDto { code, message } = error;
  CommandErrorDto::new(
    "daemon_restart_transition_failed",
    format!(
      "cooperative restart was armed after this window detached, but it could not continue ({code}): {message}",
    ),
  )
}

pub fn default_working_directory() -> Result<String, CommandErrorDto> {
  #[cfg(unix)]
  const HOME_ENVIRONMENT: &str = "HOME";
  #[cfg(windows)]
  const HOME_ENVIRONMENT: &str = "USERPROFILE";
  #[cfg(not(any(unix, windows)))]
  const HOME_ENVIRONMENT: &str = "HOME";

  let directory = std::env::var_os(HOME_ENVIRONMENT).ok_or_else(|| {
    CommandErrorDto::new(
      "home_directory_unavailable",
      "enter a working directory because the user home directory is unavailable",
    )
  })?;
  directory.into_string().map_err(|_directory| {
    CommandErrorDto::new(
      "home_directory_not_utf8",
      "enter a working directory because the user home directory is not valid UTF-8",
    )
  })
}

#[cfg(not(unix))]
pub async fn connect() -> Result<LocalStream, CommandErrorDto> {
  Err(CommandErrorDto::new(
    "unsupported_platform",
    "local rmux transport is not implemented on this platform",
  ))
}

#[cfg(not(unix))]
pub async fn preflight_restart_daemon() -> Result<RestartDaemonPreflight, CommandErrorDto> {
  Err(CommandErrorDto::new(
    "unsupported_platform",
    "local rmux transport is not implemented on this platform",
  ))
}

#[cfg(not(unix))]
pub(crate) async fn restart_daemon(
  _preflight: RestartDaemonPreflight,
) -> Result<RestartLocalDaemonOutcome, CommandErrorDto> {
  Err(CommandErrorDto::new(
    "unsupported_platform",
    "local rmux transport is not implemented on this platform",
  ))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[test]
  fn local_control_error_codes_are_stable() {
    assert_eq!(
      local_control_error_code(LocalControlErrorCode::RestartInProgress),
      "daemon_restart_in_progress"
    );
    assert_eq!(
      local_control_error_code(LocalControlErrorCode::ProtocolVersionMismatch),
      "local_control_protocol_version_mismatch"
    );
  }

  #[cfg(unix)]
  #[test]
  fn preflight_failures_are_non_destructive_but_armed_failures_are_not() {
    let protocol_mismatch = preflight_local_control_error(LocalControlClientError::Server {
      code: LocalControlErrorCode::ProtocolVersionMismatch,
      message: "local-control versions differ".into(),
    });
    assert_eq!(protocol_mismatch.code, "daemon_restart_unsupported");

    let unavailable_control = restart_preflight_failure(std::io::Error::new(
      std::io::ErrorKind::ConnectionRefused,
      "control endpoint is unavailable",
    ));
    assert_eq!(unavailable_control.code, "daemon_restart_unsupported");

    let armed_failure = restart_transition_error(restart_unsupported("request lost"));
    assert_eq!(armed_failure.code, "daemon_restart_transition_failed");
  }
}
