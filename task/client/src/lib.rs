//! Reusable local ctl-taskd transport for CLI and desktop clients.
pub mod restart;
use ctl_task_ipc::{Stream, connect};
use ctl_task_proto::{ClientMessage, ServerMessage, read_frame, write_frame};
pub use restart::{PreparedRestart, preflight_restart, preflight_restart_at, restart_daemon};
use std::{
  env, io,
  path::{Path, PathBuf},
  process::Stdio,
  time::Duration,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, sleep, timeout};
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Connects, negotiates the protocol, and sends a task request.
/// # Errors
/// Returns startup, transport, or handshake errors.
pub async fn open(request: &ClientMessage) -> Result<Stream, ClientError> {
  timeout(Duration::from_secs(10), async {
    let mut stream = connect_or_start(&ctl_task_ipc::socket_path()).await?;
    handshake(&mut stream).await?;
    write_frame(&mut stream, request).await?;
    Ok(stream)
  })
  .await
  .map_err(|_| ClientError::Timeout)?
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<(), ClientError> {
  let offer = ctl_task_proto::protocol_offer();
  write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol: offer.clone(),
      client_name: "ctl-task-client".into(),
    },
  )
  .await?;
  match read_frame(stream).await? {
    Some(ServerMessage::HandshakeAccepted {
      protocol_version,
      protocols,
    }) if offer.accepts(protocol_version)
      && ctl_core::component::protocols_are_valid(&protocols)
      && protocols
        .iter()
        .any(|protocol| protocol.name == "task" && protocol.supports(protocol_version)) =>
    {
      Ok(())
    }
    Some(ServerMessage::Error { code, message }) => Err(ClientError::Server { code, message }),
    _ => Err(ClientError::UnexpectedResponse),
  }
}

/// Exchanges one request and response.
/// # Errors
/// Returns startup, protocol, timeout, or ctl-taskd errors.
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

/// Opens the local endpoint, starting a sibling ctl-taskd when necessary.
/// # Errors
/// Returns startup or connection errors.
pub async fn connect_or_start(socket: &Path) -> Result<Stream, ClientError> {
  match connect(socket).await {
    Ok(stream) => return Ok(stream),
    Err(error) if retryable(&error) => {}
    Err(error) => return Err(ClientError::Connect(error)),
  }
  let executable = daemon_executable()?;
  spawn_daemon(socket, &executable, None)?;
  wait_for_endpoint(socket).await
}

fn spawn_daemon(
  socket: &Path,
  executable: &Path,
  configuration: Option<(&Path, &Path)>,
) -> Result<(), ClientError> {
  let mut daemon = std::process::Command::new(executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS: ctl-taskd survives the invoking console.
    daemon.creation_flags(0x0000_0008);
  }
  if let Some((data_directory, ctmux_socket)) = configuration {
    daemon
      .arg("--data-directory")
      .arg(data_directory)
      .arg("--ctmux-socket")
      .arg(ctmux_socket);
  }
  daemon
    .arg("--socket")
    .arg(socket)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ClientError::StartDaemon {
      executable: executable.to_owned(),
      source,
    })?;
  Ok(())
}

async fn wait_for_endpoint(socket: &Path) -> Result<Stream, ClientError> {
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

/// Resolves the selected task daemon executable without starting it.
///
/// # Errors
/// Returns an error if no executable can be found.
pub fn daemon_executable() -> Result<PathBuf, ClientError> {
  if let Some(executable) = env::var_os("CTL_TASKD_BIN") {
    return Ok(PathBuf::from(executable));
  }
  let current = env::current_exe().map_err(ClientError::CurrentExecutable)?;
  let sibling = current.with_file_name(format!("ctl-taskd{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return Ok(sibling);
  }
  Ok(PathBuf::from(format!(
    "ctl-taskd{}",
    env::consts::EXE_SUFFIX
  )))
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
  Codec(#[from] ctl_task_proto::CodecError),
  #[error("could not connect to ctl-taskd: {0}")]
  Connect(io::Error),
  #[error("could not determine current executable: {0}")]
  CurrentExecutable(io::Error),
  #[error("could not start ctl-taskd at {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
  #[error("{0}")]
  Restart(String),
  #[error("unexpected ctl-taskd response")]
  UnexpectedResponse,
  #[error("ctl-taskd request timed out")]
  Timeout,
  #[error("{message}")]
  Server {
    code: ctl_task_proto::ErrorCode,
    message: String,
  },
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctl_core::{component::ProtocolInfo, protocol::ProtocolVersion};

  async fn protocol_reply(
    selected: ProtocolVersion,
    protocols: Vec<ProtocolInfo>,
  ) -> Result<(), ClientError> {
    let (mut client, mut daemon) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
      assert!(
        matches!(read_frame::<_, ClientMessage>(&mut daemon).await.unwrap(),
        Some(ClientMessage::Handshake { protocol, .. }) if protocol == ctl_task_proto::protocol_offer())
      );
      write_frame(
        &mut daemon,
        &ServerMessage::HandshakeAccepted {
          protocol_version: selected,
          protocols,
        },
      )
      .await
      .unwrap();
    });
    let result = handshake(&mut client).await;
    server.await.unwrap();
    result
  }

  #[tokio::test]
  async fn handshake_accepts_shared_contract_but_rejects_unoffered_selection() {
    let latest = ProtocolVersion::new(1, 1, 6);
    let advertised = ProtocolInfo::new(
      "task",
      6,
      latest,
      &[ctl_task_proto::PROTOCOL_VERSION, latest],
    );
    protocol_reply(ctl_task_proto::PROTOCOL_VERSION, vec![advertised.clone()])
      .await
      .unwrap();
    assert!(matches!(
      protocol_reply(latest, vec![advertised]).await,
      Err(ClientError::UnexpectedResponse)
    ));
    assert!(matches!(
      protocol_reply(ctl_task_proto::PROTOCOL_VERSION, Vec::new()).await,
      Err(ClientError::UnexpectedResponse)
    ));
    let unrelated = ProtocolInfo::new("task", 6, latest, &[latest]);
    assert!(matches!(
      protocol_reply(ctl_task_proto::PROTOCOL_VERSION, vec![unrelated]).await,
      Err(ClientError::UnexpectedResponse)
    ));
  }
}
