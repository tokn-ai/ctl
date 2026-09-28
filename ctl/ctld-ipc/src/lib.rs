//! Owner-only local protocol between `ctld` and its clients.

pub mod vpn;

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
use tokio::time::{Instant, sleep, timeout};
use zeroize::{Zeroize, Zeroizing};

const DAEMON_EXECUTABLE_ENV: &str = "CTLD_BIN";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PROTOCOL_QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const MAX_FRAME_SIZE: usize = 64 * 1024;

// Version 11 adds saved VPN connection requests and explicit lifecycle states.
pub const PROTOCOL_VERSION: u16 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshGatewayMode {
  Automatic,
  NativeOnly,
  AgentRelayOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayKind {
  #[default]
  Ssh,
  Socks5,
}

// serde's skip_serializing_if callback must take a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn gateway_kind_is_ssh(kind: &GatewayKind) -> bool {
  *kind == GatewayKind::Ssh
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshGateway {
  #[serde(default, skip_serializing_if = "gateway_kind_is_ssh")]
  pub kind: GatewayKind,
  pub destination: String,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
  pub mode: SshGatewayMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SshTarget {
  pub destination: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub ssh_config_alias: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub use_ssh_config_master: Option<bool>,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
  #[serde(default)]
  pub gateways: Vec<SshGateway>,
}

/// OpenSSH expands %h and %p after parsing this option. The route contains no secrets.
///
/// # Errors
/// Returns an error if the daemon executable cannot be located.
///
/// # Panics
/// Panics if serialization of the gateway route unexpectedly fails.
pub fn proxy_command(gateways: &[SshGateway]) -> Result<String, ConnectError> {
  let executable = daemon_executable()?;
  let executable = executable.to_string_lossy().replace('\'', "'\\''");
  let bytes = serde_json::to_vec(gateways).expect("gateway route is serializable");
  let encoded = bytes
    .iter()
    .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
      use std::fmt::Write as _;
      write!(text, "{byte:02x}").expect("writing to a String cannot fail");
      text
    });
  Ok(format!(
    "'{executable}' --proxy-route {encoded} --proxy-host %h --proxy-port %p"
  ))
}

impl SshTarget {
  /// An omitted preference preserves the connection method's original policy.
  #[must_use]
  pub fn uses_ssh_config_master(&self) -> bool {
    !self
      .gateways
      .iter()
      .any(|gateway| gateway.kind == GatewayKind::Socks5)
      && self
        .use_ssh_config_master
        .unwrap_or(self.ssh_config_alias.is_some())
  }

  /// Equivalent preferences must share authentication, pause, and forward state.
  /// Omitting explicit defaults also preserves existing private socket hashes.
  pub fn normalize_master_policy(&mut self) {
    if self.use_ssh_config_master == Some(self.ssh_config_alias.is_some()) {
      self.use_ssh_config_master = None;
    }
  }
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

/// The endpoint is available only after the managed VPN and SOCKS5 listener are ready.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnStatus {
  #[serde(default)]
  pub vpn_id: Option<String>,
  pub endpoint: Option<String>,
  /// The connected gateway origin, without credentials, path, query, or fragment.
  #[serde(default)]
  pub vpn_url: Option<String>,
  #[serde(default)]
  pub username: Option<String>,
  pub container_name: Option<String>,
  pub running: bool,
  pub connection_id: Option<String>,
  pub state: VpnState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnSnapshot {
  pub connections: Vec<VpnStatus>,
  pub supports_multiple: bool,
}

impl Default for VpnSnapshot {
  fn default() -> Self {
    Self {
      connections: Vec::new(),
      supports_multiple: true,
    }
  }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnState {
  #[default]
  Stopped,
  Starting,
  Connected,
  Stopping,
}

/// Saved connection settings. Deliberately omits Debug to protect the password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnConnection {
  pub connection_id: String,
  pub name: String,
  pub url: String,
  pub username: String,
  pub password: Zeroizing<String>,
  pub auth_method: Option<String>,
  pub target_ip: Option<String>,
}

impl VpnConnection {
  /// Validates settings before persistence or conversion into a literal env file.
  ///
  /// # Errors
  /// Returns a field-only diagnostic without including any supplied values.
  pub fn validate(&self) -> Result<(), String> {
    for (name, value, maximum) in [
      ("Connection ID", self.connection_id.as_str(), 128),
      ("Name", self.name.as_str(), 256),
      ("VPN URL", self.url.as_str(), 2048),
      ("Username", self.username.as_str(), 256),
    ] {
      validate_vpn_field(name, value, maximum)?;
      if value.trim().is_empty() {
        return Err(format!("{name} is required"));
      }
    }
    validate_vpn_field("Password", &self.password, 4096)?;
    if self.password.is_empty() {
      return Err("Password is required".into());
    }
    let address = self.url.strip_prefix("https://").unwrap_or(&self.url);
    if address.contains("://")
      || self.url.chars().any(char::is_whitespace)
      || address
        .split('/')
        .next()
        .is_none_or(|host| host.is_empty() || host.contains('@'))
    {
      return Err("VPN URL must be an HTTPS gateway or bare gateway address".into());
    }
    if let Some(value) = &self.auth_method {
      validate_vpn_field("Authentication method", value, 256)?;
    }
    if let Some(value) = &self.target_ip {
      validate_vpn_field("Connectivity target", value, 64)?;
      if !value.is_empty() && value.parse::<std::net::Ipv4Addr>().is_err() {
        return Err("Connectivity target must be an IPv4 address".into());
      }
    }
    Ok(())
  }
}

fn validate_vpn_field(name: &str, value: &str, maximum: usize) -> Result<(), String> {
  if value.len() > maximum || value.contains(['\r', '\n', '\0']) {
    return Err(format!(
      "{name} must be at most {maximum} bytes and cannot contain line breaks or NUL"
    ));
  }
  Ok(())
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
  ConnectionStatus {
    target: SshTarget,
  },
  DisconnectMaster {
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
  ListRemoteListeners {
    target: SshTarget,
  },
  StartVpn {
    env_file: PathBuf,
  },
  StartVpnConnection {
    connection: VpnConnection,
  },
  VpnStatus,
  StopVpn,
  StopVpnById {
    vpn_id: String,
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
  MasterDisconnected,
  ConnectionStatus {
    connected: bool,
    manually_disconnected: bool,
  },
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
  RemoteListeners {
    catalog: ctl_proto::TcpListenerCatalog,
  },
  VpnStatus {
    status: VpnStatus,
    #[serde(default)]
    snapshot: Option<VpnSnapshot>,
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
  #[error(
    "could not verify the local protocol of ctld at {} (client requires {expected}): {source}. Rebuild or reinstall the client and ctld together, and check CTLD_BIN if it is set",
    executable.display()
  )]
  CheckDaemonProtocol {
    executable: PathBuf,
    expected: u16,
    source: io::Error,
  },
  #[error(
    "ctld at {} reports local protocol {reported}, but the client requires {expected}. Rebuild or reinstall the client and ctld together, and check CTLD_BIN if it is set",
    executable.display()
  )]
  IncompatibleDaemon {
    executable: PathBuf,
    expected: u16,
    reported: u16,
  },
}

/// Resolves the daemon endpoint, including explicit socket/runtime overrides.
#[must_use]
pub fn socket_path() -> PathBuf {
  if let Some(path) = env::var_os("CTLD_SOCKET_PATH") {
    return PathBuf::from(path);
  }
  if let Some(directory) = env::var_os("CTLD_RUNTIME_DIR") {
    return PathBuf::from(directory).join(format!("ctld-v{PROTOCOL_VERSION}.sock"));
  }
  default_socket_path()
}

/// Resolves the normal per-user endpoint without `CTLD_SOCKET_PATH` or
/// `CTLD_RUNTIME_DIR` overrides. Unix still respects `XDG_RUNTIME_DIR`.
#[must_use]
pub fn default_socket_path() -> PathBuf {
  #[cfg(unix)]
  {
    let socket_name = format!("ctld-v{PROTOCOL_VERSION}.sock");
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
  connect_or_start_daemon_at(&socket_path()).await
}

/// Connects to the selected endpoint, starting `ctld` on that exact endpoint if
/// necessary. Never falls back to another daemon endpoint.
///
/// # Errors
/// Returns connection, daemon location, protocol, or startup failures.
pub async fn connect_or_start_daemon_at(path: &Path) -> Result<Stream, ConnectError> {
  connect_or_start_daemon_at_with_executable(path, None).await
}

/// Connects to one endpoint, using the selected executable only if an owner
/// needs to be started. Existing owners are never replaced or probed on disk.
///
/// # Errors
/// Returns connection, daemon location, protocol, or startup failures.
pub async fn connect_or_start_daemon_at_with_executable(
  path: &Path,
  executable: Option<&Path>,
) -> Result<Stream, ConnectError> {
  match connect(path).await {
    Ok(stream) => return Ok(stream),
    Err(error) if retryable_connect_error(&error) => {}
    Err(error) => return Err(ConnectError::Connect(error)),
  }
  start_daemon(path, executable).await?;
  let deadline = Instant::now() + CONNECT_TIMEOUT;
  loop {
    match connect(path).await {
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
  connect_existing_at(&socket_path()).await
}

/// Connects to an already-running daemon at the selected endpoint.
///
/// # Errors
/// Returns an error when the endpoint cannot be reached. Never starts a daemon.
pub async fn connect_existing_at(path: &Path) -> Result<Stream, ConnectError> {
  connect(path).await.map_err(ConnectError::Connect)
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

async fn start_daemon(path: &Path, executable: Option<&Path>) -> Result<(), ConnectError> {
  let executable = match executable {
    Some(executable) => executable.to_path_buf(),
    None => daemon_executable()?,
  };
  check_daemon_protocol(&executable, PROTOCOL_QUERY_TIMEOUT).await?;
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
    // Broker children (including SSH askpass) must contact this same owner even
    // when the caller selected a different endpoint from its environment.
    .env("CTLD_SOCKET_PATH", path)
    .env(DAEMON_EXECUTABLE_ENV, &executable)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|source| ConnectError::StartDaemon { executable, source })?;
  Ok(())
}

async fn check_daemon_protocol(
  executable: &Path,
  query_timeout: Duration,
) -> Result<(), ConnectError> {
  let mut command = tokio::process::Command::new(executable);
  #[cfg(windows)]
  command.creation_flags(0x0800_0000);
  command
    .arg("--protocol-version")
    .env_remove("CTLD_ASKPASS")
    .stdin(Stdio::null())
    .kill_on_drop(true);
  let reported = timeout(query_timeout, command.output())
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "--protocol-version timed out"))
    .and_then(std::convert::identity)
    .and_then(|output| {
      if !output.status.success() {
        return Err(io::Error::other(format!(
          "--protocol-version failed with {}; this binary may predate protocol checks",
          output.status
        )));
      }
      parse_daemon_protocol(&output.stdout)
    })
    .map_err(|source| ConnectError::CheckDaemonProtocol {
      executable: executable.to_path_buf(),
      expected: PROTOCOL_VERSION,
      source,
    })?;
  if reported != PROTOCOL_VERSION {
    return Err(ConnectError::IncompatibleDaemon {
      executable: executable.to_path_buf(),
      expected: PROTOCOL_VERSION,
      reported,
    });
  }
  Ok(())
}

fn parse_daemon_protocol(stdout: &[u8]) -> io::Result<u16> {
  std::str::from_utf8(stdout)
    .ok()
    .and_then(|output| output.trim().parse().ok())
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidData,
        "--protocol-version did not report a numeric protocol version",
      )
    })
}

/// Resolves the daemon executable bundled beside the current client.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined.
pub fn daemon_executable() -> Result<PathBuf, ConnectError> {
  if let Some(executable) = env::var_os(DAEMON_EXECUTABLE_ENV) {
    return Ok(PathBuf::from(executable));
  }
  default_daemon_executable()
}

/// Resolves the sibling, bundled helper, or PATH daemon without `CTLD_BIN`.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined.
pub fn default_daemon_executable() -> Result<PathBuf, ConnectError> {
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

  #[test]
  fn ssh_config_origin_is_optional_and_does_not_change_managed_target_json() {
    let legacy = serde_json::json!({
      "destination": "office",
      "hostname": null,
      "user": null,
      "port": null,
      "identity_file": null,
      "gateways": []
    });
    let mut target: SshTarget = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(target.ssh_config_alias, None);
    assert_eq!(target.use_ssh_config_master, None);
    assert_eq!(serde_json::to_value(&target).unwrap(), legacy);

    target.ssh_config_alias = Some("office".into());
    let value = serde_json::to_value(&target).unwrap();
    assert_eq!(value["ssh_config_alias"], "office");
    assert_eq!(serde_json::from_value::<SshTarget>(value).unwrap(), target);
  }

  #[test]
  fn master_preferences_preserve_defaults_and_canonical_transport_identity() {
    for alias in [None, Some("office")] {
      let target: SshTarget = serde_json::from_value(serde_json::json!({
        "destination": "office",
        "ssh_config_alias": alias,
        "hostname": null,
        "user": null,
        "port": null,
        "identity_file": null
      }))
      .unwrap();
      let default = alias.is_some();
      assert_eq!(target.uses_ssh_config_master(), default);
      let legacy = serde_json::to_value(&target).unwrap();
      assert!(legacy.get("use_ssh_config_master").is_none());
      for selected in [false, true] {
        let mut explicit = target.clone();
        explicit.use_ssh_config_master = Some(selected);
        assert_eq!(explicit.uses_ssh_config_master(), selected);
        let value = serde_json::to_value(&explicit).unwrap();
        assert_eq!(value["use_ssh_config_master"], selected);
        assert_eq!(
          serde_json::from_value::<SshTarget>(value).unwrap(),
          explicit
        );
        explicit.normalize_master_policy();
        assert_eq!(explicit.uses_ssh_config_master(), selected);
        if selected == default {
          assert_eq!(explicit, target);
          assert_eq!(serde_json::to_value(&explicit).unwrap(), legacy);
        } else {
          assert_eq!(explicit.use_ssh_config_master, Some(selected));
          assert_ne!(explicit, target);
        }
      }
    }
  }

  #[test]
  fn protocol_probe_requires_one_numeric_version() {
    assert_eq!(parse_daemon_protocol(b"6\n").unwrap(), 6);
    for output in [b"".as_slice(), b"ctld 0.1.0", b"6\n5\n", b"65536", b"\xff"] {
      assert_eq!(
        parse_daemon_protocol(output).unwrap_err().kind(),
        io::ErrorKind::InvalidData
      );
    }
  }

  #[cfg(unix)]
  static PROTOCOL_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

  #[cfg(unix)]
  struct ProtocolFixture {
    directory: PathBuf,
    executable: PathBuf,
    _execution_guard: tokio::sync::MutexGuard<'static, ()>,
  }

  #[cfg(unix)]
  impl ProtocolFixture {
    async fn new(body: &str) -> Self {
      use std::os::unix::fs::PermissionsExt as _;
      use std::sync::atomic::{AtomicU64, Ordering};

      static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
      // A child can briefly inherit another fixture's writable descriptor
      // before exec. Keep fixture writes and subprocess creation serialized.
      let execution_guard = PROTOCOL_FIXTURE_LOCK.lock().await;
      let directory = env::temp_dir().join(format!(
        "ctld-protocol-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap()
          .as_nanos(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
      ));
      std::fs::create_dir(&directory).unwrap();
      let executable = directory.join("ctld");
      std::fs::write(
        &executable,
        format!("#!/bin/sh\nset -eu\n[ \"$1\" = --protocol-version ]\n{body}\n"),
      )
      .unwrap();
      std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
      Self {
        directory,
        executable,
        _execution_guard: execution_guard,
      }
    }
  }

  #[cfg(unix)]
  impl Drop for ProtocolFixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.directory);
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_accepts_matching_helper() {
    let fixture = ProtocolFixture::new(&format!("printf '%s\\n' {PROTOCOL_VERSION}")).await;
    check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_rejects_outdated_helper_with_selected_path() {
    let previous_version = PROTOCOL_VERSION - 1;
    let fixture = ProtocolFixture::new(&format!("printf '%s\\n' {previous_version}")).await;
    let error = check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap_err();
    assert!(
      matches!(
        &error,
        ConnectError::IncompatibleDaemon { executable, expected, reported }
          if executable == &fixture.executable
            && *expected == PROTOCOL_VERSION
            && *reported == previous_version
      ),
      "expected a protocol mismatch, got {error:?}"
    );
    let message = error.to_string();
    assert!(message.contains(fixture.executable.to_str().unwrap()));
    assert!(message.contains("CTLD_BIN"));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_rejects_unsupported_flag_and_invalid_output() {
    for (body, expected_kind) in [
      ("exit 2", io::ErrorKind::Other),
      ("printf 'ctld 0.1.0\\n'", io::ErrorKind::InvalidData),
    ] {
      let fixture = ProtocolFixture::new(body).await;
      let error = check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
        .await
        .unwrap_err();
      assert!(
        matches!(
          &error,
          ConnectError::CheckDaemonProtocol { executable, expected, source }
            if executable == &fixture.executable
              && *expected == PROTOCOL_VERSION
              && source.kind() == expected_kind
        ),
        "fixture {body:?} expected {expected_kind:?}, got {error:?}"
      );
      assert!(
        error.to_string().contains("--protocol-version"),
        "fixture {body:?} omitted the failed query from its diagnostic: {error:?}"
      );
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_times_out_unresponsive_helper() {
    let fixture = ProtocolFixture::new("exec sleep 30").await;
    let error = check_daemon_protocol(&fixture.executable, Duration::from_millis(100))
      .await
      .unwrap_err();
    assert!(
      matches!(
        &error,
        ConnectError::CheckDaemonProtocol { source, .. }
          if source.kind() == io::ErrorKind::TimedOut
      ),
      "expected the helper query to time out, got {error:?}"
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn pinned_daemon_bootstrap_ignores_environment_and_preserves_existing_owners() {
    for mode in ["bootstrap", "existing"] {
      let fixture = ProtocolFixture::new("exit 1").await;
      std::fs::write(
        &fixture.executable,
        format!(
          "#!/bin/sh\nset -eu\nif [ \"$1\" = --protocol-version ]; then\n  printf '%s\\n' {PROTOCOL_VERSION}\n  exit 0\nfi\nprintf '%s\\n' \"$1\" \"$2\" \"$3\" \"$CTLD_SOCKET_PATH\" \"$CTLD_BIN\" > \"$CTLD_PINNED_TEST_MARKER\"\n"
        ),
      )
      .unwrap();
      let output = timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(env::current_exe().unwrap())
          .args(["--exact", "tests::pinned_daemon_child", "--nocapture"])
          .env("CTLD_PINNED_TEST_MODE", mode)
          .env("CTLD_PINNED_TEST_EXECUTABLE", &fixture.executable)
          .env("CTLD_PINNED_TEST_MARKER", fixture.directory.join("started"))
          .env(
            DAEMON_EXECUTABLE_ENV,
            fixture.directory.join("missing-override"),
          )
          .kill_on_drop(true)
          .output(),
      )
      .await
      .unwrap()
      .unwrap();
      assert!(output.status.success(), "{mode}: {output:?}");
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn pinned_daemon_child() {
    struct Endpoint(PathBuf);
    impl Drop for Endpoint {
      fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
      }
    }
    let Ok(mode) = env::var("CTLD_PINNED_TEST_MODE") else {
      return;
    };
    let endpoint = Endpoint(PathBuf::from(format!(
      "/tmp/ctld-pinned-executable-{}.sock",
      std::process::id()
    )));
    let executable = PathBuf::from(env::var_os("CTLD_PINNED_TEST_EXECUTABLE").unwrap());
    let marker = PathBuf::from(env::var_os("CTLD_PINNED_TEST_MARKER").unwrap());
    let inherited = PathBuf::from(env::var_os(DAEMON_EXECUTABLE_ENV).unwrap());
    assert_eq!(daemon_executable().unwrap(), inherited);
    assert_ne!(default_daemon_executable().unwrap(), inherited);
    if mode == "existing" {
      let _owner = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
      connect_or_start_daemon_at_with_executable(&endpoint.0, Some(&inherited))
        .await
        .unwrap();
      assert!(!marker.exists());
      return;
    }
    let server = async {
      while !marker.exists() {
        sleep(Duration::from_millis(5)).await;
      }
      let owner = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
      owner.accept().await.unwrap();
    };
    let (connection, ()) = tokio::join!(
      connect_or_start_daemon_at_with_executable(&endpoint.0, Some(&executable)),
      server
    );
    connection.unwrap();
    let invocation = std::fs::read_to_string(marker).unwrap();
    assert_eq!(
      invocation.lines().collect::<Vec<_>>(),
      [
        "--socket",
        endpoint.0.to_str().unwrap(),
        "--detach-from-terminal",
        endpoint.0.to_str().unwrap(),
        executable.to_str().unwrap(),
      ]
    );
  }

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
