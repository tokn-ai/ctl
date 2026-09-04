//! Stateless SSH remote-command gateway for the local `rmuxd` service.
//!
//! `ctld connect` is a disposable process. It relays one SSH channel's
//! stdin/stdout to the fixed per-user `rmuxd` endpoint and owns no terminal,
//! session, authorization, or reconnect state.

use rmux_ipc::{Stream, connect_existing_daemon};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, sleep};

const RMUX_START_TIMEOUT: Duration = Duration::from_secs(3);
pub const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

#[derive(Debug, Clone)]
pub struct ConnectConfig {
  /// Fixed per-user local `rmuxd` endpoint. It is never client controlled.
  pub rmux_socket: PathBuf,
  /// Absolute installed `rmuxd` path used only when the endpoint is absent.
  pub rmuxd_bin: Option<PathBuf>,
}

impl ConnectConfig {
  #[must_use]
  pub fn new(rmux_socket: PathBuf) -> Self {
    Self {
      rmux_socket,
      rmuxd_bin: None,
    }
  }
}

/// Connects the process standard streams to the fixed local `rmuxd` endpoint.
///
/// # Errors
///
/// Returns an error when `rmuxd` cannot be reached or either relay direction
/// fails. Completion of either direction ends the entire disposable relay.
pub async fn connect_stdio(config: &ConnectConfig) -> Result<(), DaemonError> {
  connect(tokio::io::stdin(), tokio::io::stdout(), config).await
}

/// Relays a caller-supplied input/output pair to the fixed local endpoint.
///
/// This generic entry point keeps the SSH stdio boundary independently
/// testable without running an SSH server.
///
/// # Errors
///
/// Returns an error when the local daemon cannot be reached or relay I/O
/// fails.
pub async fn connect<R, W>(
  mut client_reader: R,
  mut client_writer: W,
  config: &ConnectConfig,
) -> Result<(), DaemonError>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
  let rmux = connect_or_start_rmuxd(config).await?;
  client_writer
    .write_all(SSH_TRANSPORT_PREFACE)
    .await
    .map_err(DaemonError::Relay)?;
  client_writer.flush().await.map_err(DaemonError::Relay)?;
  let (mut rmux_reader, mut rmux_writer) = tokio::io::split(rmux);
  let client_to_rmux = tokio::io::copy(&mut client_reader, &mut rmux_writer);
  let rmux_to_client = tokio::io::copy(&mut rmux_reader, &mut client_writer);
  tokio::pin!(client_to_rmux, rmux_to_client);

  tokio::select! {
    result = &mut client_to_rmux => {
      result.map_err(DaemonError::Relay)?;
    }
    result = &mut rmux_to_client => {
      result.map_err(DaemonError::Relay)?;
    }
  }
  Ok(())
}

async fn connect_or_start_rmuxd(config: &ConnectConfig) -> Result<Stream, DaemonError> {
  match connect_existing_daemon(&config.rmux_socket).await {
    Ok(stream) => return Ok(stream),
    Err(error) if error.is_endpoint_unavailable() => {}
    Err(error) => return Err(DaemonError::RmuxConnect(error)),
  }

  let executable = config
    .rmuxd_bin
    .as_deref()
    .ok_or_else(|| DaemonError::RmuxUnavailable(config.rmux_socket.clone()))?;
  start_rmuxd(executable, &config.rmux_socket)?;

  let deadline = Instant::now() + RMUX_START_TIMEOUT;
  loop {
    match connect_existing_daemon(&config.rmux_socket).await {
      Ok(stream) => return Ok(stream),
      Err(error) if Instant::now() < deadline && error.is_endpoint_unavailable() => {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(DaemonError::RmuxConnect(error)),
    }
  }
}

fn start_rmuxd(executable: &Path, socket: &Path) -> Result<(), DaemonError> {
  if !executable.is_absolute() {
    return Err(DaemonError::RmuxdPathNotAbsolute(executable.into()));
  }
  let mut command = std::process::Command::new(executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    // OpenSSH permits job breakaway. Detaching only from the console would
    // still let the SSH job kill rmuxd when this disposable channel closes.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
  }
  command
    .arg("--socket")
    .arg(socket)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| DaemonError::StartRmuxd {
      executable: executable.into(),
      source,
    })?;
  Ok(())
}

#[derive(Debug, Error)]
pub enum DaemonError {
  #[error("the local rmux service is unavailable at {}", .0.display())]
  RmuxUnavailable(PathBuf),
  #[error("could not connect to the local rmux service: {0}")]
  RmuxConnect(#[source] rmux_ipc::ConnectError),
  #[error("the rmuxd path must be absolute: {}", .0.display())]
  RmuxdPathNotAbsolute(PathBuf),
  #[error("could not start rmuxd at {}: {source}", executable.display())]
  StartRmuxd {
    executable: PathBuf,
    #[source]
    source: io::Error,
  },
  #[error("SSH/local relay I/O failed: {0}")]
  Relay(#[source] io::Error),
}
