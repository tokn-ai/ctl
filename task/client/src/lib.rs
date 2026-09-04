//! Reusable local taskd transport for CLI and desktop clients.
use std::{
  env, io,
  path::{Path, PathBuf},
  process::Stdio,
  time::Duration,
};
use task_ipc::{Stream, connect};
use task_proto::{ClientMessage, PROTOCOL_VERSION, ServerMessage, read_frame, write_frame};
use thiserror::Error;
use tokio::time::{Instant, sleep, timeout};
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects, negotiates the protocol, and sends a task request.
/// # Errors
/// Returns startup, transport, or handshake errors.
pub async fn open(request: &ClientMessage) -> Result<Stream, ClientError> {
  timeout(Duration::from_secs(10), async {
    let mut stream = connect_or_start(&task_ipc::socket_path()).await?;
    write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol_version: PROTOCOL_VERSION,
        client_name: "task-client".into(),
      },
    )
    .await?;
    match read_frame(&mut stream).await? {
      Some(ServerMessage::HandshakeAccepted { protocol_version })
        if protocol_version == PROTOCOL_VERSION => {}
      Some(ServerMessage::Error { code, message }) => {
        return Err(ClientError::Server { code, message });
      }
      _ => return Err(ClientError::UnexpectedResponse),
    }
    write_frame(&mut stream, request).await?;
    Ok(stream)
  })
  .await
  .map_err(|_| ClientError::Timeout)?
}

/// Exchanges one request and response.
/// # Errors
/// Returns startup, protocol, timeout, or taskd errors.
pub async fn request(request: &ClientMessage) -> Result<ServerMessage, ClientError> {
  let mut stream = open(request).await?;
  match timeout(Duration::from_secs(30), read_frame(&mut stream))
    .await
    .map_err(|_| ClientError::Timeout)??
  {
    Some(ServerMessage::Error { code, message }) => Err(ClientError::Server { code, message }),
    Some(response) => Ok(response),
    None => Err(ClientError::UnexpectedResponse),
  }
}

/// Opens the local endpoint, starting a sibling taskd when necessary.
/// # Errors
/// Returns startup or connection errors.
pub async fn connect_or_start(socket: &Path) -> Result<Stream, ClientError> {
  match connect(socket).await {
    Ok(stream) => return Ok(stream),
    Err(error) if retryable(&error) => {}
    Err(error) => return Err(ClientError::Connect(error)),
  }
  let executable = daemon_executable()?;
  let mut daemon = std::process::Command::new(&executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS: taskd survives the invoking console.
    daemon.creation_flags(0x0000_0008);
  }
  daemon
    .arg("--socket")
    .arg(socket)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ClientError::StartDaemon { executable, source })?;
  let deadline = Instant::now() + CONNECT_TIMEOUT;
  loop {
    match connect(socket).await {
      Ok(stream) => return Ok(stream),
      Err(error) if retryable(&error) && Instant::now() < deadline => {
        sleep(Duration::from_millis(25)).await;
      }
      Err(error) => return Err(ClientError::Connect(error)),
    }
  }
}

fn daemon_executable() -> Result<PathBuf, ClientError> {
  if let Some(executable) = env::var_os("TASKD_BIN") {
    return Ok(PathBuf::from(executable));
  }
  let current = env::current_exe().map_err(ClientError::CurrentExecutable)?;
  let sibling = current.with_file_name(format!("taskd{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return Ok(sibling);
  }
  Ok(PathBuf::from(format!("taskd{}", env::consts::EXE_SUFFIX)))
}

fn retryable(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

#[derive(Debug, Error)]
pub enum ClientError {
  #[error(transparent)]
  Codec(#[from] task_proto::CodecError),
  #[error("could not connect to taskd: {0}")]
  Connect(io::Error),
  #[error("could not determine current executable: {0}")]
  CurrentExecutable(io::Error),
  #[error("could not start taskd at {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
  #[error("unexpected taskd response")]
  UnexpectedResponse,
  #[error("taskd request timed out")]
  Timeout,
  #[error("{message}")]
  Server {
    code: task_proto::ErrorCode,
    message: String,
  },
}
