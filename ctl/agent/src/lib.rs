//! Stateless SSH remote-command gateway for local ctl services.
//!
//! `ctl-agent connect` is a disposable process. It relays one SSH channel's
//! stdin/stdout to the fixed per-user `rmuxd` endpoint (or `taskd` with
//! `--service task`) and owns no terminal, task, or reconnect state.

use rmux_ipc::Stream;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, sleep};

const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(3);
pub const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

/// Services exposed by the SSH gateway. Local rmux control is never exposed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Service {
  #[default]
  Rmux,
  Task,
}

#[derive(Debug, Clone)]
pub struct ConnectConfig {
  pub service: Service,
  /// Fixed per-user local `rmuxd` endpoint. It is never client controlled.
  pub rmux_socket: PathBuf,
  /// Absolute installed `rmuxd` path used only when the endpoint is absent.
  pub rmuxd_bin: Option<PathBuf>,
  /// Fixed per-user local `taskd` endpoint. It is never client controlled.
  pub task_socket: PathBuf,
  /// Absolute installed `taskd` path used only when the endpoint is absent.
  pub taskd_bin: Option<PathBuf>,
}

impl ConnectConfig {
  #[must_use]
  pub fn new(rmux_socket: PathBuf) -> Self {
    Self {
      service: Service::Rmux,
      rmux_socket,
      rmuxd_bin: None,
      task_socket: task_ipc::socket_path(),
      taskd_bin: None,
    }
  }

  fn socket(&self) -> &Path {
    match self.service {
      Service::Rmux => &self.rmux_socket,
      Service::Task => &self.task_socket,
    }
  }

  fn executable(&self) -> Result<&Path, AgentError> {
    match self.service {
      Service::Rmux => self
        .rmuxd_bin
        .as_deref()
        .ok_or_else(|| AgentError::RmuxUnavailable(self.rmux_socket.clone())),
      Service::Task => self
        .taskd_bin
        .as_deref()
        .ok_or_else(|| AgentError::TaskUnavailable(self.task_socket.clone())),
    }
  }
}

/// Connects the process standard streams to the selected fixed local endpoint.
///
/// # Errors
///
/// Returns an error when the daemon cannot be reached or either relay direction
/// fails. Completion of either direction ends the entire disposable relay.
pub async fn connect_stdio(config: &ConnectConfig) -> Result<(), AgentError> {
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
) -> Result<(), AgentError>
where
  R: AsyncRead + Unpin,
  W: AsyncWrite + Unpin,
{
  let daemon = connect_or_start_daemon(config).await?;
  client_writer
    .write_all(SSH_TRANSPORT_PREFACE)
    .await
    .map_err(AgentError::Relay)?;
  client_writer.flush().await.map_err(AgentError::Relay)?;
  let (mut daemon_reader, mut daemon_writer) = tokio::io::split(daemon);
  let client_to_daemon = tokio::io::copy(&mut client_reader, &mut daemon_writer);
  let daemon_to_client = tokio::io::copy(&mut daemon_reader, &mut client_writer);
  tokio::pin!(client_to_daemon, daemon_to_client);

  tokio::select! {
    result = &mut client_to_daemon => {
      result.map_err(AgentError::Relay)?;
    }
    result = &mut daemon_to_client => {
      result.map_err(AgentError::Relay)?;
    }
  }
  Ok(())
}

async fn connect_existing_daemon(config: &ConnectConfig) -> Result<Stream, AgentError> {
  match config.service {
    Service::Rmux => rmux_ipc::connect_existing_daemon(config.socket())
      .await
      .map_err(AgentError::RmuxConnect),
    Service::Task => task_ipc::connect(config.socket())
      .await
      .map_err(AgentError::TaskConnect),
  }
}

async fn connect_or_start_daemon(config: &ConnectConfig) -> Result<Stream, AgentError> {
  match connect_existing_daemon(config).await {
    Ok(stream) => return Ok(stream),
    Err(error) if error.is_endpoint_unavailable() => {}
    Err(error) => return Err(error),
  }

  start_daemon(config)?;

  let deadline = Instant::now() + DAEMON_START_TIMEOUT;
  loop {
    match connect_existing_daemon(config).await {
      Ok(stream) => return Ok(stream),
      Err(error) if Instant::now() < deadline && error.is_endpoint_unavailable() => {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(error),
    }
  }
}

fn daemon_command(config: &ConnectConfig) -> Result<std::process::Command, AgentError> {
  let executable = config.executable()?;
  if !executable.is_absolute() {
    return Err(match config.service {
      Service::Rmux => AgentError::RmuxdPathNotAbsolute(executable.into()),
      Service::Task => AgentError::TaskdPathNotAbsolute(executable.into()),
    });
  }
  let mut command = std::process::Command::new(executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    // OpenSSH permits job breakaway. Detaching only from the console would
    // still let the SSH job kill the daemon when this channel closes.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
  }
  command
    .arg("--socket")
    .arg(config.socket())
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
  if config.service == Service::Task {
    command.arg("--rmux-socket").arg(&config.rmux_socket);
  }
  Ok(command)
}

fn start_daemon(config: &ConnectConfig) -> Result<(), AgentError> {
  let mut command = daemon_command(config)?;
  let executable = PathBuf::from(command.get_program());
  command.spawn().map_err(|source| match config.service {
    Service::Rmux => AgentError::StartRmuxd { executable, source },
    Service::Task => AgentError::StartTaskd { executable, source },
  })?;
  Ok(())
}

#[derive(Debug, Error)]
pub enum AgentError {
  #[error("the local task service is unavailable at {} (install taskd beside ctl-agent)", .0.display())]
  TaskUnavailable(PathBuf),
  #[error("could not connect to the local task service: {0}")]
  TaskConnect(#[source] io::Error),
  #[error("the taskd path must be absolute: {}", .0.display())]
  TaskdPathNotAbsolute(PathBuf),
  #[error("could not start taskd at {}: {source}", executable.display())]
  StartTaskd {
    executable: PathBuf,
    #[source]
    source: io::Error,
  },
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

impl AgentError {
  fn is_endpoint_unavailable(&self) -> bool {
    match self {
      Self::RmuxConnect(error) => error.is_endpoint_unavailable(),
      Self::TaskConnect(error) => matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
      ),
      _ => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn task_daemon_starts_detached_with_the_gateway_rmux_endpoint() {
    let mut config = ConnectConfig::new(rmux_ipc::socket_path());
    config.service = Service::Task;
    // current_exe is absolute on every supported platform.
    config.taskd_bin = Some(std::env::current_exe().unwrap());
    let command = daemon_command(&config).unwrap();
    assert_eq!(command.get_program(), config.taskd_bin.as_ref().unwrap());
    assert_eq!(
      command.get_args().collect::<Vec<_>>(),
      [
        std::ffi::OsStr::new("--socket"),
        config.task_socket.as_os_str(),
        std::ffi::OsStr::new("--detach-from-terminal"),
        std::ffi::OsStr::new("--rmux-socket"),
        config.rmux_socket.as_os_str(),
      ]
    );
  }

  #[test]
  fn task_daemon_never_uses_a_relative_executable() {
    let mut config = ConnectConfig::new(rmux_ipc::socket_path());
    config.service = Service::Task;
    config.taskd_bin = Some("taskd".into());
    assert!(matches!(
      daemon_command(&config),
      Err(AgentError::TaskdPathNotAbsolute(_))
    ));
  }

  #[test]
  fn task_connect_only_starts_a_daemon_for_an_absent_endpoint() {
    for kind in [io::ErrorKind::NotFound, io::ErrorKind::ConnectionRefused] {
      assert!(AgentError::TaskConnect(io::Error::from(kind)).is_endpoint_unavailable());
    }
    assert!(
      !AgentError::TaskConnect(io::Error::from(io::ErrorKind::PermissionDenied))
        .is_endpoint_unavailable()
    );
  }
}
