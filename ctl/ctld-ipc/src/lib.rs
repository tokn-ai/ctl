//! Owner-only local protocol between `ctld` and its clients.

use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[cfg(windows)]
pub use interprocess::local_socket::tokio::Stream;
#[cfg(windows)]
use interprocess::local_socket::{GenericFilePath, ToFsName, traits::tokio::Stream as _};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
#[cfg(unix)]
pub use tokio::net::UnixStream as Stream;
use tokio::time::{Instant, sleep};
use zeroize::{Zeroize, Zeroizing};

const DAEMON_EXECUTABLE_ENV: &str = "CTLD_BIN";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const MAX_FRAME_SIZE: usize = 64 * 1024;

pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshTarget {
  pub destination: String,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalPortForward {
  pub forward_id: String,
  pub bind_address: String,
  pub local_port: u16,
  pub remote_host: String,
  pub remote_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortForwardState {
  WaitingForAuthentication,
  Active,
  Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortForwardStatus {
  pub forward: LocalPortForward,
  pub state: PortForwardState,
  pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
  Confirm,
  Secret,
  CredentialSave,
  CredentialSaveError,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  Handshake {
    protocol_version: u16,
  },
  EnsureMaster {
    target: SshTarget,
  },
  PromptResponse {
    prompt_id: String,
    response: Option<Zeroizing<String>>,
  },
  Askpass {
    token: String,
    message: String,
    confirm: bool,
  },
  MasterStatus {
    target: SshTarget,
  },
  DeleteCredentials {
    target: SshTarget,
  },
  ConfigurePortForward {
    target: SshTarget,
    forward: LocalPortForward,
    enabled: bool,
  },
  ListPortForwards {
    target: SshTarget,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted {
    protocol_version: u16,
  },
  Prompt {
    prompt_id: String,
    kind: PromptKind,
    message: String,
  },
  MasterReady {
    control_path: PathBuf,
  },
  AuthenticationRequired,
  AskpassResponse {
    response: Option<Zeroizing<String>>,
  },
  CredentialsDeleted,
  PortForwardConfigured {
    status: PortForwardStatus,
  },
  PortForwards {
    statuses: Vec<PortForwardStatus>,
  },
  Error {
    code: String,
    message: String,
  },
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
  #[error("ctld I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("ctld frame length {actual} exceeds the maximum of {maximum} bytes")]
  FrameTooLarge { actual: usize, maximum: usize },
  #[error("invalid ctld JSON frame: {0}")]
  Json(#[from] serde_json::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
  #[error("could not connect to ctld: {0}")]
  Connect(#[source] io::Error),
  #[error("could not determine the current executable: {0}")]
  CurrentExecutable(#[source] io::Error),
  #[error("could not start ctld using {}: {source}", executable.display())]
  StartDaemon {
    executable: PathBuf,
    source: io::Error,
  },
}

#[must_use]
pub fn socket_path() -> PathBuf {
  if let Some(path) = env::var_os("CTLD_SOCKET_PATH") {
    return PathBuf::from(path);
  }
  let socket_name = format!("ctld-v{PROTOCOL_VERSION}.sock");
  if let Some(directory) = env::var_os("CTLD_RUNTIME_DIR") {
    return PathBuf::from(directory).join(socket_name);
  }
  #[cfg(unix)]
  {
    if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
      return PathBuf::from(directory).join("ctld").join(socket_name);
    }
    let uid = rustix::process::getuid().as_raw();
    PathBuf::from("/tmp")
      .join(format!("ctld-{uid}"))
      .join(socket_name)
  }
  #[cfg(windows)]
  {
    use std::os::windows::ffi::OsStrExt as _;
    let directory = dirs::data_local_dir().unwrap_or_else(env::temp_dir);
    let bytes: Vec<u8> = directory
      .as_os_str()
      .encode_wide()
      .flat_map(u16::to_le_bytes)
      .collect();
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &bytes);
    PathBuf::from(format!(r"\\.\pipe\ctld-v{PROTOCOL_VERSION}-{id}"))
  }
}

/// Connects to the per-user daemon, starting its sibling executable if needed.
///
/// # Errors
/// Returns an error when the endpoint cannot be reached or `ctld` cannot be
/// located and started.
pub async fn connect_or_start_daemon() -> Result<Stream, ConnectError> {
  let path = socket_path();
  match connect(&path).await {
    Ok(stream) => return Ok(stream),
    Err(error) if retryable_connect_error(&error) => {}
    Err(error) => return Err(ConnectError::Connect(error)),
  }
  start_daemon(&path)?;
  let deadline = Instant::now() + CONNECT_TIMEOUT;
  loop {
    match connect(&path).await {
      Ok(stream) => return Ok(stream),
      Err(error) if retryable_connect_error(&error) && Instant::now() < deadline => {
        sleep(CONNECT_RETRY_INTERVAL).await;
      }
      Err(error) => return Err(ConnectError::Connect(error)),
    }
  }
}

/// Connects to an already-running per-user daemon.
///
/// # Errors
/// Returns an error when the endpoint cannot be reached.
pub async fn connect_existing() -> Result<Stream, ConnectError> {
  connect(&socket_path()).await.map_err(ConnectError::Connect)
}

/// Writes one length-delimited protocol message.
///
/// # Errors
/// Returns an error when serialization fails, the encoded frame is too large,
/// or the stream cannot be written.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), CodecError>
where
  W: AsyncWrite + Unpin,
  T: Serialize,
{
  let payload = Zeroizing::new(serde_json::to_vec(message)?);
  if payload.len() > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: payload.len(),
      maximum: MAX_FRAME_SIZE,
    });
  }
  #[allow(clippy::cast_possible_truncation)]
  let length = payload.len() as u32;
  writer.write_all(&length.to_be_bytes()).await?;
  writer.write_all(&payload).await?;
  writer.flush().await?;
  Ok(())
}

/// Reads one length-delimited protocol message, or `None` at a clean EOF.
///
/// # Errors
/// Returns an error when the frame is malformed, too large, or cannot be read.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, CodecError>
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
  if length > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: length,
      maximum: MAX_FRAME_SIZE,
    });
  }
  let mut payload = Zeroizing::new(vec![0_u8; length]);
  reader.read_exact(&mut payload).await?;
  let message = serde_json::from_slice(&payload)?;
  payload.zeroize();
  Ok(Some(message))
}

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

fn start_daemon(path: &Path) -> Result<(), ConnectError> {
  let executable = daemon_executable()?;
  let mut command = std::process::Command::new(&executable);
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x0000_0008);
  }
  command
    .arg("--socket")
    .arg(path)
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
  let sibling = current_executable.with_file_name(format!("ctld{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return Ok(sibling);
  }
  #[cfg(target_os = "macos")]
  if let Some(helper) = bundled_macos_daemon(&current_executable)
    && helper.is_file()
  {
    return Ok(helper);
  }
  Ok(PathBuf::from(format!("ctld{}", env::consts::EXE_SUFFIX)))
}

#[cfg(target_os = "macos")]
fn bundled_macos_daemon(current_executable: &Path) -> Option<PathBuf> {
  let contents = current_executable.parent()?.parent()?;
  Some(contents.join("Helpers/ctld.app/Contents/MacOS/ctld"))
}

fn retryable_connect_error(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(target_os = "macos")]
  #[test]
  fn locates_ctld_in_the_macos_helper_bundle() {
    let executable = Path::new("/Applications/rmux.app/Contents/MacOS/rmux");
    assert_eq!(
      bundled_macos_daemon(executable),
      Some(PathBuf::from(
        "/Applications/rmux.app/Contents/Helpers/ctld.app/Contents/MacOS/ctld"
      ))
    );
  }

  #[tokio::test]
  async fn frames_round_trip_secret_responses() {
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    let write = tokio::spawn(async move {
      write_frame(
        &mut writer,
        &ClientMessage::PromptResponse {
          prompt_id: "prompt".into(),
          response: Some(Zeroizing::new("synthetic-secret".into())),
        },
      )
      .await
      .unwrap();
    });
    match read_frame::<_, ClientMessage>(&mut reader).await.unwrap() {
      Some(ClientMessage::PromptResponse {
        prompt_id,
        response: Some(response),
      }) => {
        assert_eq!(prompt_id, "prompt");
        assert_eq!(response.as_str(), "synthetic-secret");
      }
      _ => panic!("unexpected frame"),
    }
    write.await.unwrap();
  }

  #[tokio::test]
  async fn oversized_frames_are_rejected_before_allocation() {
    let oversized = u32::try_from(MAX_FRAME_SIZE).unwrap() + 1;
    let mut encoded = &oversized.to_be_bytes()[..];
    assert!(matches!(
      read_frame::<_, ClientMessage>(&mut encoded).await,
      Err(CodecError::FrameTooLarge { .. })
    ));
  }
}
