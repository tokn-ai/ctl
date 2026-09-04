use std::env;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
pub use interprocess::local_socket::tokio::Stream;
#[cfg(windows)]
use interprocess::local_socket::{GenericFilePath, ToFsName, traits::tokio::Stream as _};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(unix)]
pub use tokio::net::UnixStream as Stream;
use tokio::time::{Instant, sleep};
#[cfg(windows)]
pub mod windows;

async fn connect(path: &Path) -> io::Result<Stream> {
  #[cfg(unix)]
  {
    Stream::connect(path).await
  }
  #[cfg(windows)]
  {
    Stream::connect(path.to_fs_name::<GenericFilePath>()?).await
  }
}

const RUNTIME_DIRECTORY_ENV: &str = "RMUX_RUNTIME_DIR";
const DAEMON_EXECUTABLE_ENV: &str = "RMUXD_BIN";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const MAX_LOCAL_CONTROL_FRAME_SIZE: usize = 64 * 1024;

/// Version of the owner-only local `rmuxd` control endpoint.
///
/// This protocol is intentionally separate from `rmux-proto`: `ctl-agent` relays
/// only the ordinary data endpoint and must never expose daemon-global local
/// maintenance operations to remote `rmux_tunnel` clients.
pub const LOCAL_CONTROL_PROTOCOL_VERSION: u16 = 1;

#[must_use]
pub fn socket_path() -> PathBuf {
  #[cfg(unix)]
  {
    runtime_directory().join("rmux.sock")
  }
  #[cfg(windows)]
  {
    windows::endpoint(&runtime_directory())
  }
}

/// Returns the owner-only local-control endpoint associated with a data
/// endpoint.
///
/// Deriving the path from the selected data endpoint keeps custom `--socket`
/// invocations isolated from one another while ensuring the control endpoint
/// shares the same validated runtime directory.
///
/// # Errors
///
/// Returns an error if `socket_path` has no final path component.
pub fn control_socket_path(socket_path: &Path) -> io::Result<PathBuf> {
  let file_name = socket_path.file_name().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!(
        "rmux data endpoint {} has no final path component",
        socket_path.display()
      ),
    )
  })?;
  let mut control_file_name = file_name.to_os_string();
  control_file_name.push(".control");
  Ok(socket_path.with_file_name(control_file_name))
}

#[must_use]
pub fn runtime_directory() -> PathBuf {
  if let Some(directory) = env::var_os(RUNTIME_DIRECTORY_ENV) {
    return PathBuf::from(directory);
  }

  if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
    return PathBuf::from(directory).join("rmux");
  }

  fallback_runtime_directory()
}

/// Request messages accepted only by `rmuxd`'s owner-only local-control
/// endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalControlClientMessage {
  Handshake { protocol_version: u16 },
  RestartDaemon,
}

/// Response messages emitted by `rmuxd`'s owner-only local-control endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalControlServerMessage {
  HandshakeAccepted {
    protocol_version: u16,
    restart_supported: bool,
  },
  RestartAccepted {
    terminated_sessions: u32,
  },
  Error {
    code: LocalControlErrorCode,
    message: String,
  },
}

/// Stable errors for the owner-only local-control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalControlErrorCode {
  InvalidRequest,
  ProtocolVersionMismatch,
  RestartUnsupported,
  RestartInProgress,
  Internal,
}

/// Capabilities negotiated with the owner-only local-control endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalControlCapabilities {
  pub restart_supported: bool,
}

/// Errors while framing the owner-only local-control protocol.
#[derive(Debug, thiserror::Error)]
pub enum LocalControlCodecError {
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("local-control frame length {actual} exceeds the maximum of {maximum} bytes")]
  FrameTooLarge { actual: usize, maximum: usize },
  #[error("invalid local-control JSON frame: {0}")]
  Json(#[from] serde_json::Error),
}

/// Client-side errors for owner-only local-control operations.
#[derive(Debug, thiserror::Error)]
pub enum LocalControlClientError {
  #[error(transparent)]
  Codec(#[from] LocalControlCodecError),
  #[error("the running rmuxd does not support cooperative restart")]
  RestartUnsupported,
  #[error("local-control server error {code:?}: {message}")]
  Server {
    code: LocalControlErrorCode,
    message: String,
  },
  #[error("expected local-control {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: &'static str,
  },
}

/// Writes one length-prefixed local-control frame.
///
/// # Errors
///
/// Returns an error when serialization fails, the frame is oversized, or the
/// transport cannot be written or flushed.
pub async fn write_local_control_frame<W, T>(
  writer: &mut W,
  message: &T,
) -> Result<(), LocalControlCodecError>
where
  W: AsyncWrite + Unpin,
  T: Serialize,
{
  let payload = serde_json::to_vec(message)?;
  if payload.len() > MAX_LOCAL_CONTROL_FRAME_SIZE {
    return Err(LocalControlCodecError::FrameTooLarge {
      actual: payload.len(),
      maximum: MAX_LOCAL_CONTROL_FRAME_SIZE,
    });
  }

  #[allow(clippy::cast_possible_truncation)]
  let length = payload.len() as u32;
  writer.write_all(&length.to_be_bytes()).await?;
  writer.write_all(&payload).await?;
  writer.flush().await?;
  Ok(())
}

/// Reads one length-prefixed local-control frame.
///
/// A clean end of stream before a new frame returns `Ok(None)`.
///
/// # Errors
///
/// Returns an error when the transport fails mid-frame, the declared frame is
/// too large, or its payload is not valid JSON for `T`.
pub async fn read_local_control_frame<R, T>(
  reader: &mut R,
) -> Result<Option<T>, LocalControlCodecError>
where
  R: AsyncRead + Unpin,
  T: DeserializeOwned,
{
  let mut length_bytes = [0_u8; 4];
  match reader.read(&mut length_bytes[..1]).await {
    Ok(0) => return Ok(None),
    Ok(_) => {
      reader.read_exact(&mut length_bytes[1..]).await?;
    }
    Err(error) => return Err(error.into()),
  }

  let length = u32::from_be_bytes(length_bytes) as usize;
  if length > MAX_LOCAL_CONTROL_FRAME_SIZE {
    return Err(LocalControlCodecError::FrameTooLarge {
      actual: length,
      maximum: MAX_LOCAL_CONTROL_FRAME_SIZE,
    });
  }

  let mut payload = vec![0_u8; length];
  reader.read_exact(&mut payload).await?;
  Ok(Some(serde_json::from_slice(&payload)?))
}

/// Negotiates local-control capabilities with a connected daemon.
///
/// The caller retains the stream so it can decide whether to issue a later
/// maintenance operation. A missing control endpoint is intentionally not
/// translated here: callers need to distinguish a legacy running daemon from
/// an endpoint that is simply absent.
///
/// # Errors
///
/// Returns an error when the handshake fails, the daemon rejects it, or its
/// response is malformed.
pub async fn local_control_handshake<S>(
  stream: &mut S,
) -> Result<LocalControlCapabilities, LocalControlClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  write_local_control_frame(
    stream,
    &LocalControlClientMessage::Handshake {
      protocol_version: LOCAL_CONTROL_PROTOCOL_VERSION,
    },
  )
  .await?;

  match read_local_control_frame(stream).await? {
    Some(LocalControlServerMessage::HandshakeAccepted {
      protocol_version,
      restart_supported,
    }) if protocol_version == LOCAL_CONTROL_PROTOCOL_VERSION => {
      Ok(LocalControlCapabilities { restart_supported })
    }
    Some(LocalControlServerMessage::Error { code, message }) => {
      Err(LocalControlClientError::Server { code, message })
    }
    Some(response) => Err(LocalControlClientError::UnexpectedResponse {
      expected: "handshake_accepted",
      actual: local_control_response_name(&response),
    }),
    None => Err(LocalControlClientError::UnexpectedResponse {
      expected: "handshake_accepted",
      actual: "end_of_stream",
    }),
  }
}

/// Requests a daemon-coordinated restart over an owner-only control stream.
///
/// This function handshakes before sending `restart_daemon`; an endpoint that
/// does not advertise support is rejected locally and never receives an
/// unknown maintenance request.
///
/// # Errors
///
/// Returns an error when the handshake fails, restart is unsupported, or the
/// daemon rejects or cannot complete the request.
pub async fn request_local_daemon_restart<S>(mut stream: S) -> Result<u32, LocalControlClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let capabilities = local_control_handshake(&mut stream).await?;
  if !capabilities.restart_supported {
    return Err(LocalControlClientError::RestartUnsupported);
  }

  request_local_daemon_restart_after_handshake(&mut stream).await
}

/// Sends `restart_daemon` over a stream whose successful local-control
/// handshake already advertised restart support.
///
/// GUI callers retain this stream across their preflight-to-detach transition
/// so they cannot detach a live view and then discover a different endpoint
/// has no restart capability.
///
/// # Errors
///
/// Returns an error when the daemon rejects the request or does not return a
/// valid restart result.
pub async fn request_local_daemon_restart_after_handshake<S>(
  stream: &mut S,
) -> Result<u32, LocalControlClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  write_local_control_frame(stream, &LocalControlClientMessage::RestartDaemon).await?;
  match read_local_control_frame(stream).await? {
    Some(LocalControlServerMessage::RestartAccepted {
      terminated_sessions,
    }) => Ok(terminated_sessions),
    Some(LocalControlServerMessage::Error { code, message }) => {
      Err(LocalControlClientError::Server { code, message })
    }
    Some(response) => Err(LocalControlClientError::UnexpectedResponse {
      expected: "restart_accepted",
      actual: local_control_response_name(&response),
    }),
    None => Err(LocalControlClientError::UnexpectedResponse {
      expected: "restart_accepted",
      actual: "end_of_stream",
    }),
  }
}

fn local_control_response_name(response: &LocalControlServerMessage) -> &'static str {
  match response {
    LocalControlServerMessage::HandshakeAccepted { .. } => "handshake_accepted",
    LocalControlServerMessage::RestartAccepted { .. } => "restart_accepted",
    LocalControlServerMessage::Error { .. } => "error",
  }
}

/// Connects to the local daemon, starting it when the endpoint is absent.
///
/// Concurrent callers may each launch a daemon candidate. Every caller then
/// waits for whichever candidate binds the endpoint first; losing candidates
/// exit when they observe that the endpoint is already served.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be connected, the daemon
/// executable cannot be resolved, or a daemon candidate cannot be spawned.
pub async fn connect_or_start_daemon(socket_path: &Path) -> Result<Stream, ConnectError> {
  connect_or_start_with(
    || connect(socket_path),
    || start_daemon(socket_path),
    CONNECT_TIMEOUT,
    CONNECT_RETRY_INTERVAL,
  )
  .await
}

/// Connects to the local daemon without starting a replacement.
///
/// This is intended for lifecycle operations which must first communicate with
/// the daemon already serving the endpoint. In particular, callers must not
/// use [`connect_or_start_daemon`] until an existing daemon has released its
/// endpoint or has been confirmed absent.
///
/// # Errors
///
/// Returns an error when no daemon is currently reachable at the endpoint.
pub async fn connect_existing_daemon(socket_path: &Path) -> Result<Stream, ConnectError> {
  connect(socket_path).await.map_err(ConnectError::Connect)
}

/// Waits until no daemon is reachable at a local endpoint.
///
/// This function only probes the endpoint. It never removes a socket file,
/// signals a process, or otherwise forces a daemon to stop.
///
/// # Errors
///
/// Returns an error when the endpoint stays reachable until `wait_timeout`,
/// or when probing it fails for a reason other than an absent endpoint.
pub async fn wait_for_daemon_drain(
  socket_path: &Path,
  wait_timeout: Duration,
) -> Result<(), EndpointDrainError> {
  wait_for_endpoint_drain_with(
    || connect(socket_path),
    wait_timeout,
    CONNECT_RETRY_INTERVAL,
  )
  .await
}

/// Waits until both endpoints owned by one `rmuxd` instance are unavailable.
///
/// A replacement must not be started after only the data endpoint disappears:
/// the exiting daemon may still own its paired local-control endpoint. This
/// function only probes the endpoints; it never unlinks sockets or signals a
/// process.
///
/// # Errors
///
/// Returns an error when either endpoint stays reachable until `wait_timeout`
/// or when a probe fails for a reason other than endpoint absence.
pub async fn wait_for_daemon_shutdown(
  data_socket_path: &Path,
  control_socket_path: &Path,
  wait_timeout: Duration,
) -> Result<(), EndpointDrainError> {
  let deadline = Instant::now() + wait_timeout;
  loop {
    let data_is_gone = endpoint_is_unavailable(data_socket_path).await?;
    let control_is_gone = endpoint_is_unavailable(control_socket_path).await?;
    if data_is_gone && control_is_gone {
      return Ok(());
    }

    if Instant::now() >= deadline {
      return Err(EndpointDrainError::TimedOut {
        timeout: wait_timeout,
      });
    }
    sleep(CONNECT_RETRY_INTERVAL).await;
  }
}

async fn connect_or_start_with<T, Connect, ConnectFuture, Start>(
  mut connect: Connect,
  start: Start,
  retry_timeout: Duration,
  retry_interval: Duration,
) -> Result<T, ConnectError>
where
  Connect: FnMut() -> ConnectFuture,
  ConnectFuture: Future<Output = io::Result<T>>,
  Start: FnOnce() -> Result<(), ConnectError>,
{
  match connect().await {
    Ok(connection) => return Ok(connection),
    Err(error) if retryable_connect_error(&error) => {}
    Err(error) => return Err(ConnectError::Connect(error)),
  }

  start()?;
  let deadline = Instant::now() + retry_timeout;
  loop {
    match connect().await {
      Ok(connection) => return Ok(connection),
      Err(error) if Instant::now() < deadline && retryable_connect_error(&error) => {
        sleep(retry_interval).await;
      }
      Err(error) => return Err(ConnectError::Connect(error)),
    }
  }
}

async fn wait_for_endpoint_drain_with<T, Connect, ConnectFuture>(
  mut connect: Connect,
  wait_timeout: Duration,
  retry_interval: Duration,
) -> Result<(), EndpointDrainError>
where
  Connect: FnMut() -> ConnectFuture,
  ConnectFuture: Future<Output = io::Result<T>>,
{
  let deadline = Instant::now() + wait_timeout;
  loop {
    match connect().await {
      Ok(connection) => drop(connection),
      Err(error) if retryable_connect_error(&error) => return Ok(()),
      Err(error) => return Err(EndpointDrainError::Connect(error)),
    }

    if Instant::now() >= deadline {
      return Err(EndpointDrainError::TimedOut {
        timeout: wait_timeout,
      });
    }
    sleep(retry_interval).await;
  }
}

async fn endpoint_is_unavailable(socket_path: &Path) -> Result<bool, EndpointDrainError> {
  match connect(socket_path).await {
    Ok(connection) => {
      drop(connection);
      Ok(false)
    }
    Err(error) if retryable_connect_error(&error) => Ok(true),
    Err(error) => Err(EndpointDrainError::Connect(error)),
  }
}

fn retryable_connect_error(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

fn start_daemon(socket_path: &Path) -> Result<(), ConnectError> {
  let executable = daemon_executable()?;
  let mut command = std::process::Command::new(&executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0000_0008); // DETACHED_PROCESS
  }
  command
    .arg("--socket")
    .arg(socket_path)
    .arg("--detach-from-terminal")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ConnectError::StartDaemon { executable, source })?;
  Ok(())
}

fn daemon_executable() -> Result<PathBuf, ConnectError> {
  if let Some(executable) = env::var_os(DAEMON_EXECUTABLE_ENV) {
    return Ok(PathBuf::from(executable));
  }

  let current_executable = env::current_exe().map_err(ConnectError::CurrentExecutable)?;
  let sibling = current_executable.with_file_name(format!("rmuxd{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return Ok(sibling);
  }

  Ok(PathBuf::from(format!("rmuxd{}", env::consts::EXE_SUFFIX)))
}

/// Failure to connect to or launch the local `rmuxd` process.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
  #[error("could not connect to rmuxd: {0}")]
  Connect(#[source] io::Error),
  #[error("could not determine the current executable: {0}")]
  CurrentExecutable(#[source] io::Error),
  #[error("could not start daemon using {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
}

impl ConnectError {
  /// Returns whether the endpoint was absent or no process was serving it.
  #[must_use]
  pub fn is_endpoint_unavailable(&self) -> bool {
    matches!(self, Self::Connect(error) if retryable_connect_error(error))
  }
}

/// Failure while waiting for an existing `rmuxd` endpoint to drain.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EndpointDrainError {
  #[error("rmuxd did not release the local endpoint within {timeout:?}")]
  TimedOut { timeout: Duration },
  #[error("could not probe the rmuxd endpoint: {0}")]
  Connect(#[source] io::Error),
}

/// Creates and validates the private directory containing a local endpoint.
///
/// # Errors
///
/// Returns an error when the endpoint has no parent, the directory cannot be
/// created, or the directory is not private and owned by the current user.
#[cfg(unix)]
pub fn prepare_runtime_directory(path: &Path) -> io::Result<()> {
  let directory = path.parent().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("socket path {} has no parent directory", path.display()),
    )
  })?;
  let existed = directory.exists();
  std::fs::create_dir_all(directory)?;
  if !existed {
    set_owner_only_permissions(directory)?;
  }
  secure_runtime_directory(directory)
}

#[cfg(unix)]
fn fallback_runtime_directory() -> PathBuf {
  let uid = rustix::process::getuid().as_raw();
  PathBuf::from("/tmp").join(format!("rmux-{uid}"))
}

#[cfg(not(unix))]
fn fallback_runtime_directory() -> PathBuf {
  dirs::data_local_dir()
    .unwrap_or_else(env::temp_dir)
    .join("ctl/rmux")
}

#[cfg(unix)]
fn secure_runtime_directory(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};

  let metadata = std::fs::symlink_metadata(directory)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime path {} is not a real directory",
        directory.display()
      ),
    ));
  }

  let expected_uid = rustix::process::getuid().as_raw();
  if metadata.uid() != expected_uid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is owned by another user",
        directory.display()
      ),
    ));
  }

  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is accessible by other users",
        directory.display()
      ),
    ));
  }

  Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt;

  std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[cfg(unix)]
  use std::sync::Arc;
  #[cfg(unix)]
  use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

  #[cfg(unix)]
  #[test]
  fn socket_is_inside_runtime_directory() {
    assert_eq!(socket_path().file_name().unwrap(), "rmux.sock");
  }

  #[cfg(unix)]
  #[test]
  fn control_socket_is_derived_from_the_selected_data_socket() {
    let data_socket = PathBuf::from("/private/runtime/custom-rmux.sock");
    assert_eq!(
      control_socket_path(&data_socket).unwrap(),
      PathBuf::from("/private/runtime/custom-rmux.sock.control")
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn restart_request_refuses_a_control_endpoint_without_capability() {
    let (client, mut daemon) = tokio::io::duplex(1024);
    let server = tokio::spawn(async move {
      let handshake: LocalControlClientMessage = read_local_control_frame(&mut daemon)
        .await
        .unwrap()
        .expect("client sends local-control handshake");
      assert_eq!(
        handshake,
        LocalControlClientMessage::Handshake {
          protocol_version: LOCAL_CONTROL_PROTOCOL_VERSION,
        }
      );
      write_local_control_frame(
        &mut daemon,
        &LocalControlServerMessage::HandshakeAccepted {
          protocol_version: LOCAL_CONTROL_PROTOCOL_VERSION,
          restart_supported: false,
        },
      )
      .await
      .unwrap();

      let next: Option<LocalControlClientMessage> =
        read_local_control_frame(&mut daemon).await.unwrap();
      assert!(
        next.is_none(),
        "unsupported endpoint must not receive restart"
      );
    });

    let error = request_local_daemon_restart(client)
      .await
      .expect_err("unsupported control capability must refuse restart");
    assert!(matches!(error, LocalControlClientError::RestartUnsupported));
    server.await.unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn healthy_endpoint_does_not_start_daemon() {
    let connect_count = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::new(AtomicUsize::new(0));
    let connection = connect_or_start_with(
      {
        let connect_count = Arc::clone(&connect_count);
        move || {
          connect_count.fetch_add(1, Ordering::Relaxed);
          std::future::ready(Ok::<_, io::Error>(42))
        }
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::ZERO,
      Duration::ZERO,
    )
    .await
    .expect("healthy endpoint connects");

    assert_eq!(connection, 42);
    assert_eq!(connect_count.load(Ordering::Relaxed), 1);
    assert_eq!(start_count.load(Ordering::Relaxed), 0);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn missing_endpoint_starts_daemon_then_retries() {
    let connect_count = Arc::new(AtomicUsize::new(0));
    let start_count = Arc::new(AtomicUsize::new(0));
    let connection = connect_or_start_with(
      {
        let connect_count = Arc::clone(&connect_count);
        move || {
          let attempt = connect_count.fetch_add(1, Ordering::Relaxed);
          std::future::ready(if attempt == 0 {
            Err(io::Error::from(io::ErrorKind::NotFound))
          } else {
            Ok(42)
          })
        }
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::from_secs(1),
      Duration::ZERO,
    )
    .await
    .expect("started endpoint connects");

    assert_eq!(connection, 42);
    assert_eq!(connect_count.load(Ordering::Relaxed), 2);
    assert_eq!(start_count.load(Ordering::Relaxed), 1);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn non_retryable_connect_error_does_not_start_daemon() {
    let start_count = Arc::new(AtomicUsize::new(0));
    let error = connect_or_start_with(
      || {
        std::future::ready(Err::<(), _>(io::Error::from(
          io::ErrorKind::PermissionDenied,
        )))
      },
      {
        let start_count = Arc::clone(&start_count);
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          Ok(())
        }
      },
      Duration::ZERO,
      Duration::ZERO,
    )
    .await
    .expect_err("permission denial must be returned");

    assert!(matches!(
      error,
      ConnectError::Connect(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(start_count.load(Ordering::Relaxed), 0);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn endpoint_drain_waits_until_the_endpoint_becomes_unavailable() {
    let connect_count = Arc::new(AtomicUsize::new(0));

    wait_for_endpoint_drain_with(
      {
        let connect_count = Arc::clone(&connect_count);
        move || {
          let attempt = connect_count.fetch_add(1, Ordering::Relaxed);
          std::future::ready(if attempt == 0 {
            Ok::<_, io::Error>(())
          } else {
            Err(io::Error::from(io::ErrorKind::ConnectionRefused))
          })
        }
      },
      Duration::from_secs(1),
      Duration::ZERO,
    )
    .await
    .expect("endpoint becomes unavailable");

    assert_eq!(connect_count.load(Ordering::Relaxed), 2);
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn endpoint_drain_times_out_without_forcing_a_live_endpoint() {
    let error = wait_for_endpoint_drain_with(
      || std::future::ready(Ok::<_, io::Error>(())),
      Duration::ZERO,
      Duration::ZERO,
    )
    .await
    .expect_err("live endpoint must not be forced closed");

    assert!(matches!(
      error,
      EndpointDrainError::TimedOut {
        timeout: Duration::ZERO
      }
    ));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn endpoint_drain_stops_on_an_unexpected_probe_error() {
    let error = wait_for_endpoint_drain_with(
      || {
        std::future::ready(Err::<(), _>(io::Error::from(
          io::ErrorKind::PermissionDenied,
        )))
      },
      Duration::from_secs(1),
      Duration::ZERO,
    )
    .await
    .expect_err("permission errors require user-visible failure");

    assert!(matches!(
      error,
      EndpointDrainError::Connect(error) if error.kind() == io::ErrorKind::PermissionDenied
    ));
  }

  #[cfg(unix)]
  #[test]
  fn only_absent_endpoints_are_safe_to_replace() {
    assert!(
      ConnectError::Connect(io::Error::from(io::ErrorKind::NotFound)).is_endpoint_unavailable()
    );
    assert!(
      ConnectError::Connect(io::Error::from(io::ErrorKind::ConnectionRefused))
        .is_endpoint_unavailable()
    );
    assert!(
      !ConnectError::Connect(io::Error::from(io::ErrorKind::PermissionDenied))
        .is_endpoint_unavailable()
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn concurrent_connectors_tolerate_duplicate_start_candidates() {
    fn connect(
      barrier: Arc<tokio::sync::Barrier>,
      endpoint_ready: Arc<AtomicBool>,
      start_count: Arc<AtomicUsize>,
    ) -> impl Future<Output = Result<(), ConnectError>> {
      let connect_count = Arc::new(AtomicUsize::new(0));
      let connect_endpoint_ready = Arc::clone(&endpoint_ready);
      connect_or_start_with(
        move || {
          let first_attempt = connect_count.fetch_add(1, Ordering::Relaxed) == 0;
          let barrier = Arc::clone(&barrier);
          let endpoint_ready = Arc::clone(&connect_endpoint_ready);
          async move {
            if first_attempt {
              barrier.wait().await;
              Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            } else if endpoint_ready.load(Ordering::Acquire) {
              Ok(())
            } else {
              Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            }
          }
        },
        move || {
          start_count.fetch_add(1, Ordering::Relaxed);
          endpoint_ready.store(true, Ordering::Release);
          Ok(())
        },
        Duration::from_secs(1),
        Duration::ZERO,
      )
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let endpoint_ready = Arc::new(AtomicBool::new(false));
    let start_count = Arc::new(AtomicUsize::new(0));
    let first = connect(
      Arc::clone(&barrier),
      Arc::clone(&endpoint_ready),
      Arc::clone(&start_count),
    );
    let second = connect(barrier, endpoint_ready, Arc::clone(&start_count));

    let (first, second) = tokio::join!(first, second);
    first.expect("first connector observes the endpoint");
    second.expect("second connector observes the endpoint");
    assert_eq!(start_count.load(Ordering::Relaxed), 2);
  }
}
