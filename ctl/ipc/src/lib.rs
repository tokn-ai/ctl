//! Owner-only local protocol between `ctld` and its clients.

pub mod credentials;
pub mod identities;
pub mod lifecycle;
pub mod managed;
pub mod remote_vpn;
#[cfg(unix)]
pub mod stdio;
pub mod vpn;
mod vpn_config;
pub use vpn_config::{VpnConnection, VpnProvider, VpnSettings};

use ctl_core::component::ComponentInfo;
#[cfg(test)]
use ctl_core::component::ProtocolInfo;
use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use std::collections::HashMap;
use std::env;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::OnceLock;
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
const MAX_COMPONENT_INFO_BYTES: usize = 16 * 1024;

/// Lazy discovery and preparation supplied by a daemon client.
pub type DaemonExecutableFuture = Pin<Box<dyn Future<Output = io::Result<Option<PathBuf>>> + Send>>;
pub type DaemonExecutableProvider = fn() -> DaemonExecutableFuture;
/// A provider that can select a helper for a specific advertised contract.
pub type ContractDaemonExecutableProvider = fn(Option<ProtocolVersion>) -> DaemonExecutableFuture;

static DAEMON_PROVIDER: OnceLock<DaemonProvider> = OnceLock::new();

struct DaemonProvider {
  callback: Box<dyn Fn(Option<ProtocolVersion>) -> DaemonExecutableFuture + Send + Sync>,
  policy: DaemonDiscoveryPolicy,
  executable: OnceLock<PathBuf>,
  preparing: tokio::sync::Mutex<HashMap<Option<ProtocolVersion>, PathBuf>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DaemonDiscoveryPolicy {
  Desktop,
  Shared,
  Embedded,
  Preferred,
}

impl DaemonProvider {
  fn new(callback: impl Fn() -> DaemonExecutableFuture + Send + Sync + 'static) -> Self {
    Self::with_policy(callback, DaemonDiscoveryPolicy::Desktop)
  }

  fn with_policy(
    callback: impl Fn() -> DaemonExecutableFuture + Send + Sync + 'static,
    policy: DaemonDiscoveryPolicy,
  ) -> Self {
    Self::with_contract_policy(move |_| callback(), policy)
  }

  fn with_contract_policy(
    callback: impl Fn(Option<ProtocolVersion>) -> DaemonExecutableFuture + Send + Sync + 'static,
    policy: DaemonDiscoveryPolicy,
  ) -> Self {
    Self {
      callback: Box::new(callback),
      policy,
      executable: OnceLock::new(),
      preparing: tokio::sync::Mutex::new(HashMap::new()),
    }
  }

  #[cfg(all(test, unix))]
  async fn prepare(&self) -> io::Result<Option<PathBuf>> {
    self.prepare_for(None).await
  }

  async fn prepare_for(&self, required: Option<ProtocolVersion>) -> io::Result<Option<PathBuf>> {
    let mut prepared = self.preparing.lock().await;
    if let Some(executable) = prepared.get(&required) {
      return Ok(Some(executable.clone()));
    }
    if required.is_none()
      && self.policy == DaemonDiscoveryPolicy::Preferred
      && let Some(executable) = self.executable.get()
    {
      let executable = executable.clone();
      prepared.insert(required, executable.clone());
      return Ok(Some(executable));
    }
    let executable = (self.callback)(required).await?;
    if let Some(executable) = &executable {
      prepared.insert(required, executable.clone());
      // Release/desktop operation helpers cannot replace their default broker.
      // Preferred discovery verifies every returned path's broker/lifecycle
      // contracts too, so its first verified helper can serve sync lookup.
      if required.is_none() || self.policy == DaemonDiscoveryPolicy::Preferred {
        let _ = self.executable.set(executable.clone());
      }
    }
    Ok(executable)
  }
}

/// Registers one process-local provider without preparing or executing a helper.
/// Existing owners and passive observations do not consult the provider. Clients
/// must verify their payload before returning an executable; returning `None`
/// preserves ordinary daemon discovery. A nearby desktop bundle keeps priority.
/// A successful preparation is reused for this process, while failed or cancelled
/// preparation can be retried.
///
/// # Errors
/// Returns an error if a provider has already been registered in this process.
pub fn register_daemon_executable_provider(provider: DaemonExecutableProvider) -> io::Result<()> {
  register_provider(DaemonProvider::new(provider))
}

/// Registers standalone CLI discovery before any nearby desktop bundle.
/// The callback must verify compatibility and trust before returning a shared
/// installation or preparing its embedded payload. Returning `None` falls back
/// to a desktop bundle, sibling executable, or PATH; it does not select a managed
/// installation without the callback's verification. Registration is lazy, so
/// explicit overrides, existing owners, and passive observations are unaffected.
///
/// # Errors
/// Returns an error if a provider has already been registered in this process.
pub fn register_standalone_daemon_executable_provider(
  provider: DaemonExecutableProvider,
) -> io::Result<()> {
  register_provider(DaemonProvider::with_policy(
    provider,
    DaemonDiscoveryPolicy::Shared,
  ))
}

/// Registers standalone discovery that receives each operation's helper contract.
/// The provider must verify both trust and the requested capability. Existing
/// daemon owners and explicit `CTLD_BIN` overrides retain their priority.
///
/// # Errors
/// Returns an error if another provider is already registered.
pub fn register_contract_daemon_executable_provider(
  provider: ContractDaemonExecutableProvider,
) -> io::Result<()> {
  register_provider(DaemonProvider::with_contract_policy(
    provider,
    DaemonDiscoveryPolicy::Shared,
  ))
}

/// Registers a verified local preference before shared and desktop selections.
/// Unlike an embedded development payload, returning `None` permits ordinary
/// fallback. Trust failures remain errors. Explicit `CTLD_BIN` overrides retain
/// priority, and successful default preparation is reused by synchronous lookup.
/// Every returned helper must also satisfy the broker and lifecycle contracts,
/// allowing a capability-specific preparation to populate default discovery.
///
/// # Errors
/// Returns an error if another provider is already registered.
pub fn register_preferred_contract_daemon_executable_provider(
  provider: ContractDaemonExecutableProvider,
) -> io::Result<()> {
  register_provider(DaemonProvider::with_contract_policy(
    provider,
    DaemonDiscoveryPolicy::Preferred,
  ))
}

/// Registers a signed development CLI's matching embedded helper. Preparation
/// takes precedence over shared and desktop selections; failure never falls
/// back to another build. Explicit `CTLD_BIN` overrides still take priority.
///
/// # Errors
/// Returns an error if another provider is already registered.
pub fn register_development_daemon_executable_provider(
  provider: ContractDaemonExecutableProvider,
) -> io::Result<()> {
  register_provider(DaemonProvider::with_contract_policy(
    provider,
    DaemonDiscoveryPolicy::Embedded,
  ))
}

fn register_provider(provider: DaemonProvider) -> io::Result<()> {
  DAEMON_PROVIDER.set(provider).map_err(|_| {
    io::Error::new(
      io::ErrorKind::AlreadyExists,
      "a ctld provider is already registered",
    )
  })
}

/// Internal evolution counter; advancing it alone does not publish a contract.
pub const PROTOCOL_BUILD: u16 = 13;
pub const CONTRACT_V1_0_12: ProtocolVersion = ProtocolVersion::new(1, 0, 12);
pub const CONTRACT_V1_1_13: ProtocolVersion = ProtocolVersion::new(1, 1, 13);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_1_13;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_12, CONTRACT_V1_1_13];

#[must_use]
pub fn protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}

/// Internal build of the one-shot credential, identity, askpass, and proxy APIs.
/// Published helper contracts are independent of the broker and lifecycle APIs.
pub const HELPER_API_BUILD: u16 = 4;
pub const HELPER_API_CONTRACT_V1_0_1: ProtocolVersion = ProtocolVersion::new(1, 0, 1);
pub const HELPER_API_CONTRACT_V1_1_2: ProtocolVersion = ProtocolVersion::new(1, 1, 2);
pub const HELPER_API_CONTRACT_V1_1_3: ProtocolVersion = ProtocolVersion::new(1, 1, 3);
pub const HELPER_API_CONTRACT_V1_1_4: ProtocolVersion = ProtocolVersion::new(1, 1, 4);
pub const HELPER_API_VERSION: ProtocolVersion = HELPER_API_CONTRACT_V1_1_4;
pub const SUPPORTED_HELPER_API_VERSIONS: &[ProtocolVersion] = &[
  HELPER_API_CONTRACT_V1_0_1,
  HELPER_API_CONTRACT_V1_1_2,
  HELPER_API_CONTRACT_V1_1_3,
  HELPER_API_CONTRACT_V1_1_4,
];

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
  Vpn,
}

impl GatewayKind {
  #[must_use]
  pub fn requires_proxy_command(self) -> bool {
    matches!(self, Self::Socks5 | Self::Vpn)
  }
}

/// A stable VPN reference. Its route position selects the execution host.
/// The current SOCKS5 endpoint is deliberately resolved only when connecting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VpnGateway {
  pub connection_id: String,
  pub socket_path: PathBuf,
  /// Pin the preceding SSH host's account environment when it has been verified.
  /// The socket path is used only for a first, local VPN step.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expected_remote_id: Option<String>,
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
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub vpn: Option<VpnGateway>,
  pub destination: String,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
  pub mode: SshGatewayMode,
}

impl SshGateway {
  /// Native `ProxyJump` cannot apply a `HostName` override while keeping an alias.
  #[must_use]
  pub fn requires_proxy_command(&self) -> bool {
    self.kind.requires_proxy_command() || self.hostname.is_some()
  }

  /// VPN references cannot also contain a stale endpoint or SSH credentials.
  #[must_use]
  pub fn has_valid_vpn_configuration(&self) -> bool {
    match (&self.kind, &self.vpn) {
      (GatewayKind::Vpn, Some(vpn)) => {
        !vpn.connection_id.is_empty()
          && vpn.connection_id.len() <= 128
          && !vpn
            .connection_id
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
          && vpn.socket_path.is_absolute()
          && vpn
            .expected_remote_id
            .as_deref()
            .is_none_or(ctl_proto::valid_remote_id)
          && self.destination == vpn.connection_id
          && self.hostname.is_none()
          && self.user.is_none()
          && self.port.is_none()
          && self.identity_file.is_none()
          && self.mode == SshGatewayMode::Automatic
      }
      (GatewayKind::Vpn, None) => false,
      (_, vpn) => vpn.is_none(),
    }
  }
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

/// VPN nesting is unchanged: a VPN is local first, or follows an SSH hop.
#[must_use]
pub fn has_valid_gateway_route(gateways: &[SshGateway]) -> bool {
  gateways.len() <= 8
    && gateways.iter().enumerate().all(|(index, gateway)| {
      gateway.has_valid_vpn_configuration()
        && (gateway.kind != GatewayKind::Vpn
          || if index == 0 {
            gateway
              .vpn
              .as_ref()
              .is_none_or(|vpn| vpn.expected_remote_id.is_none())
          } else {
            gateways[index - 1].kind == GatewayKind::Ssh
          })
    })
}

/// Remote execution was added in the published broker contract 1.1.13.
#[must_use]
pub fn has_remote_vpn(gateways: &[SshGateway]) -> bool {
  gateways
    .iter()
    .enumerate()
    .any(|(index, gateway)| index != 0 && gateway.kind == GatewayKind::Vpn)
}

/// Old contracts retain local VPN routing; remote routes require explicit support.
#[must_use]
pub fn gateway_route_supported(gateways: &[SshGateway], protocol: ProtocolVersion) -> bool {
  SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol)
    && (!has_remote_vpn(gateways) || protocol >= CONTRACT_V1_1_13)
}

/// Resolve a remote VPN's SSH owner using the exact prefix that reaches it.
#[must_use]
pub fn vpn_owner_target(gateways: &[SshGateway], vpn_index: usize) -> Option<SshTarget> {
  if gateways.get(vpn_index)?.kind != GatewayKind::Vpn {
    return None;
  }
  let owner_index = vpn_index.checked_sub(1)?;
  let owner = gateways.get(owner_index)?;
  if owner.kind != GatewayKind::Ssh {
    return None;
  }
  let mut target = SshTarget {
    destination: owner.destination.clone(),
    // A HostName override must retain the alias used when preparing this owner.
    ssh_config_alias: owner.hostname.as_ref().map(|_| owner.destination.clone()),
    use_ssh_config_master: Some(false),
    hostname: owner.hostname.clone(),
    user: owner.user.clone(),
    port: owner.port,
    identity_file: owner.identity_file.clone(),
    gateways: gateways[..owner_index].to_vec(),
  };
  target.normalize_master_policy();
  Some(target)
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
  Ok(proxy_command_with_executable(gateways, &executable))
}

/// Prepares the default helper before creating a fresh SOCKS/VPN SSH route.
///
/// # Errors
/// Returns daemon discovery or bundled-helper preparation failures.
pub async fn prepare_proxy_command(gateways: &[SshGateway]) -> Result<String, ConnectError> {
  let executable = prepare_daemon_executable().await?;
  if has_remote_vpn(gateways) {
    check_remote_proxy_helper(&executable).await?;
  }
  Ok(proxy_command_with_executable(gateways, &executable))
}

/// Formats a proxy route pinned to an already selected helper executable.
/// Daemon children use their own executable to keep nested routes on the same
/// verified helper without repeating discovery or consulting inherited overrides.
///
/// # Panics
/// Panics if serialization of the gateway route unexpectedly fails.
#[must_use]
pub fn proxy_command_with_executable(gateways: &[SshGateway], executable: &Path) -> String {
  let executable = executable.to_string_lossy().replace('\'', "'\\''");
  let bytes = serde_json::to_vec(gateways).expect("gateway route is serializable");
  let encoded = bytes
    .iter()
    .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
      use std::fmt::Write as _;
      write!(text, "{byte:02x}").expect("writing to a String cannot fail");
      text
    });
  format!("'{executable}' --proxy-route {encoded} --proxy-host %h --proxy-port %p")
}

impl SshTarget {
  /// An omitted preference preserves the connection method's original policy.
  #[must_use]
  pub fn uses_ssh_config_master(&self) -> bool {
    !self.gateways.iter().any(SshGateway::requires_proxy_command)
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
  pub provider: VpnProvider,
  /// Short-lived browser sign-in URL, never persisted in a saved profile.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub auth_url: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub message: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hostname: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tailnet: Option<String>,
  #[serde(default)]
  pub vpn_id: Option<String>,
  pub endpoint: Option<String>,
  /// The connected gateway origin, without credentials, path, query, or fragment.
  #[serde(default)]
  pub vpn_url: Option<String>,
  #[serde(default)]
  pub username: Option<String>,
  pub container_name: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub container_id: Option<String>,
  /// The container accepts independent heartbeat interests from multiple daemons.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub shared_container: bool,
  /// Whether this daemon currently keeps the shared container alive.
  /// Older daemons omit this field and retain their original ownership behavior.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub locally_connected: Option<bool>,
  /// The retained metadata has not been verified against the container engine.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub status_unavailable: bool,
  pub running: bool,
  pub connection_id: Option<String>,
  pub state: VpnState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnSnapshot {
  pub connections: Vec<VpnStatus>,
  pub supports_multiple: bool,
  #[serde(default = "legacy_vpn_providers")]
  pub supported_providers: Vec<VpnProvider>,
  #[serde(default)]
  pub supports_tailscale_enrollment: bool,
  /// Container inventory failures; healthy local connections remain visible.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub discovery_warnings: Vec<String>,
}

fn legacy_vpn_providers() -> Vec<VpnProvider> {
  vec![VpnProvider::Openconnect]
}

impl Default for VpnSnapshot {
  fn default() -> Self {
    Self {
      connections: Vec::new(),
      supports_multiple: true,
      supported_providers: vec![VpnProvider::Openconnect, VpnProvider::Tailscale],
      supports_tailscale_enrollment: true,
      discovery_warnings: Vec::new(),
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
    protocol: ProtocolOffer,
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
  ForgetTailscaleIdentity {
    connection_id: String,
  },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted {
    protocol_version: ProtocolVersion,
  },
  Prompt {
    prompt_id: String,
    kind: PromptKind,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
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
    status: Box<VpnStatus>,
    #[serde(default)]
    snapshot: Option<VpnSnapshot>,
  },
  VpnIdentityForgotten,
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
  #[error("could not prepare the bundled ctld: {0}")]
  PrepareDaemon(#[source] io::Error),
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
    expected: ProtocolVersion,
    source: io::Error,
  },
  #[error(
    "ctld at {} reports local protocol {reported}, but the client requires {expected}. Rebuild or reinstall the client and ctld together, and check CTLD_BIN if it is set",
    executable.display()
  )]
  IncompatibleDaemon {
    executable: PathBuf,
    expected: ProtocolVersion,
    reported: ProtocolVersion,
  },
  #[error(
    "ctld helper at {executable} does not support remote VPN routes; update it to helper contract {required}"
  )]
  UnsupportedProxyRoute {
    executable: PathBuf,
    required: ProtocolVersion,
  },
}

/// Resolves the daemon endpoint, including explicit socket/runtime overrides.
#[must_use]
pub fn socket_path() -> PathBuf {
  if let Some(path) = env::var_os("CTLD_SOCKET_PATH") {
    return PathBuf::from(path);
  }
  if let Some(directory) = env::var_os("CTLD_RUNTIME_DIR") {
    return PathBuf::from(directory).join(format!("ctld-v{}.sock", PROTOCOL_VERSION.major));
  }
  default_socket_path()
}

/// Resolves the normal per-user endpoint without `CTLD_SOCKET_PATH` or
/// `CTLD_RUNTIME_DIR` overrides. Unix still respects `XDG_RUNTIME_DIR`.
#[must_use]
pub fn default_socket_path() -> PathBuf {
  #[cfg(unix)]
  {
    let socket_name = format!("ctld-v{}.sock", PROTOCOL_VERSION.major);
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
    PathBuf::from(format!(r"\\.\pipe\ctld-v{}-{id}", PROTOCOL_VERSION.major))
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
  let selected = match executable {
    Some(executable) => executable.to_path_buf(),
    None => prepare_daemon_executable().await?,
  };
  // A staged symlink may change between the protocol query and startup. Pin
  // both launches, and the helper's future children, to the same build.
  let executable = resolve_executable(&selected).map_err(|source| ConnectError::StartDaemon {
    executable: selected,
    source,
  })?;
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

fn resolve_executable(selected: &Path) -> io::Result<PathBuf> {
  let path = if selected.components().count() == 1 {
    let mut inaccessible = false;
    env::var_os("PATH")
      .into_iter()
      .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
      .map(|directory| directory.join(selected))
      .find(|path| {
        if !path.is_file() {
          return false;
        }
        let executable = {
          #[cfg(unix)]
          {
            rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
          }
          #[cfg(not(unix))]
          {
            true
          }
        };
        if !executable {
          inaccessible = true;
        }
        executable
      })
      .ok_or_else(|| {
        io::Error::new(
          if inaccessible {
            io::ErrorKind::PermissionDenied
          } else {
            io::ErrorKind::NotFound
          },
          if cfg!(target_os = "macos") {
            "ctld was not found as an executable on PATH; run `ctl setup` to install the signed macOS helper"
          } else {
            "ctld was not found as an executable on PATH"
          },
        )
      })?
  } else {
    selected.to_owned()
  };
  path.canonicalize()
}

async fn check_daemon_protocol(
  executable: &Path,
  query_timeout: Duration,
) -> Result<(), ConnectError> {
  let reported = query_daemon_metadata(executable, query_timeout)
    .await
    .and_then(|metadata| {
      metadata
        .protocols
        .into_iter()
        .find(|entry| entry.name == "ctld")
        .ok_or_else(|| {
          io::Error::new(
            io::ErrorKind::InvalidData,
            "--component-info omitted the ctld protocol",
          )
        })
    })
    .map_err(|source| ConnectError::CheckDaemonProtocol {
      executable: executable.to_path_buf(),
      expected: PROTOCOL_VERSION,
      source,
    })?;
  if reported.negotiate(SUPPORTED_PROTOCOL_VERSIONS).is_none() {
    return Err(ConnectError::IncompatibleDaemon {
      executable: executable.to_path_buf(),
      expected: PROTOCOL_VERSION,
      reported: reported.version,
    });
  }
  Ok(())
}

async fn check_remote_proxy_helper(executable: &Path) -> Result<(), ConnectError> {
  let metadata = query_daemon_metadata(executable, PROTOCOL_QUERY_TIMEOUT)
    .await
    .map_err(ConnectError::PrepareDaemon)?;
  if metadata.protocols.iter().any(|protocol| {
    protocol.name == "ctld_helper"
      && protocol
        .supported_versions
        .contains(&HELPER_API_CONTRACT_V1_1_2)
  }) {
    return Ok(());
  }
  Err(ConnectError::UnsupportedProxyRoute {
    executable: executable.to_path_buf(),
    required: HELPER_API_CONTRACT_V1_1_2,
  })
}

async fn query_daemon_metadata(
  executable: &Path,
  query_timeout: Duration,
) -> io::Result<ComponentInfo> {
  timeout(query_timeout, async {
    let mut command = tokio::process::Command::new(executable);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
      .arg("--component-info")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()?;
    let mut output = Vec::new();
    child
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("missing --component-info output"))?
      .take(MAX_COMPONENT_INFO_BYTES as u64 + 1)
      .read_to_end(&mut output)
      .await?;
    if output.len() > MAX_COMPONENT_INFO_BYTES {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "--component-info returned oversized metadata",
      ));
    }
    let status = child.wait().await?;
    if !status.success() {
      return Err(io::Error::other(format!(
        "--component-info failed with {status}; this binary may predate published protocol checks"
      )));
    }
    parse_daemon_metadata(&output)
  })
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "--component-info timed out"))?
}

fn parse_daemon_metadata(stdout: &[u8]) -> io::Result<ComponentInfo> {
  let invalid = || {
    io::Error::new(
      io::ErrorKind::InvalidData,
      "--component-info did not report valid protocol metadata",
    )
  };
  if stdout.len() > MAX_COMPONENT_INFO_BYTES {
    return Err(invalid());
  }
  let metadata: ComponentInfo = serde_json::from_slice(stdout).map_err(|_| invalid())?;
  if !metadata.is_valid() {
    return Err(invalid());
  }
  Ok(metadata)
}

#[cfg(test)]
fn parse_daemon_protocol(stdout: &[u8]) -> io::Result<ProtocolInfo> {
  parse_daemon_metadata(stdout)?
    .protocols
    .into_iter()
    .find(|entry| entry.name == "ctld")
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidData,
        "--component-info omitted the ctld protocol",
      )
    })
}

/// Resolves an explicit `CTLD_BIN` override or the default daemon executable.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined or a
/// managed macOS installation is invalid.
pub fn daemon_executable() -> Result<PathBuf, ConnectError> {
  if let Some(executable) = env::var_os(DAEMON_EXECUTABLE_ENV) {
    return Ok(PathBuf::from(executable));
  }
  default_daemon_executable()
}

/// Resolves the selected daemon, lazily discovering or preparing a helper.
/// Explicit overrides retain priority. Desktop clients prefer their signed
/// bundle; release providers verify shared installations first. Signed
/// development providers prepare their matching embedded helper first. On macOS,
/// unsafe managed selections fail before release provider preparation. This function
/// never starts or stops a daemon.
///
/// # Errors
/// Returns discovery, unsafe managed selection, or provider preparation errors.
pub async fn prepare_daemon_executable() -> Result<PathBuf, ConnectError> {
  prepare_daemon_with_contract(None).await
}

/// Selects a helper for an operation requiring one advertised helper contract.
/// Capability-aware providers may prepare their verified bundled helper when a
/// shared installation is too old. Explicit `CTLD_BIN` overrides remain
/// authoritative; release clients also preserve explicit complete selections.
/// Callers must inspect the returned executable before sending the operation.
/// This never starts, stops, or restarts a broker.
///
/// # Errors
/// Returns discovery, trust, or preparation errors.
pub async fn prepare_daemon_executable_for_helper_contract(
  required: ProtocolVersion,
) -> Result<PathBuf, ConnectError> {
  prepare_daemon_with_contract(Some(required)).await
}

async fn prepare_daemon_with_contract(
  required: Option<ProtocolVersion>,
) -> Result<PathBuf, ConnectError> {
  if let Some(executable) = env::var_os(DAEMON_EXECUTABLE_ENV) {
    return Ok(PathBuf::from(executable));
  }
  let current_executable = env::current_exe().map_err(ConnectError::CurrentExecutable)?;
  prepare_default_daemon_for_contract(
    &current_executable,
    dirs::home_dir().as_deref(),
    DAEMON_PROVIDER.get(),
    required,
  )
  .await
}

#[cfg(all(test, unix))]
async fn prepare_default_daemon(
  current_executable: &Path,
  home: Option<&Path>,
  provider: Option<&DaemonProvider>,
) -> Result<PathBuf, ConnectError> {
  prepare_default_daemon_for_contract(current_executable, home, provider, None).await
}

async fn prepare_default_daemon_for_contract(
  current_executable: &Path,
  home: Option<&Path>,
  provider: Option<&DaemonProvider>,
  required: Option<ProtocolVersion>,
) -> Result<PathBuf, ConnectError> {
  if let Some(provider) =
    provider.filter(|provider| provider.policy == DaemonDiscoveryPolicy::Preferred)
    && let Some(executable) = provider
      .prepare_for(required)
      .await
      .map_err(ConnectError::PrepareDaemon)?
  {
    return Ok(executable);
  }
  if let Some(provider) =
    provider.filter(|provider| provider.policy == DaemonDiscoveryPolicy::Embedded)
  {
    return provider
      .prepare_for(required)
      .await
      .map_err(ConnectError::PrepareDaemon)?
      .ok_or_else(|| {
        ConnectError::PrepareDaemon(io::Error::other(
          "the signed development CLI has no matching embedded ctld helper",
        ))
      });
  }
  #[cfg(unix)]
  if let Some(executable) = selected_bundle_daemon(home)? {
    #[cfg(target_os = "macos")]
    {
      let provider = provider.ok_or_else(|| {
        ConnectError::PrepareDaemon(io::Error::other(
          "selected macOS bundles require a verifying helper provider",
        ))
      })?;
      let _preparing = provider.preparing.lock().await;
      // A complete build selection is explicit. Verify that exact build;
      // operation preflight reports missing capabilities without replacing it.
      let verified = (provider.callback)(None)
        .await
        .map_err(ConnectError::PrepareDaemon)?;
      if verified.as_ref() != Some(&executable) {
        return Err(ConnectError::PrepareDaemon(io::Error::other(
          "selected helper changed during verification",
        )));
      }
    }
    return Ok(executable);
  }
  #[cfg(target_os = "macos")]
  {
    if !shared_first(provider)
      && let Some(helper) = bundled_macos_daemon(current_executable)
      && helper.is_file()
    {
      return Ok(helper);
    }
    validate_managed_selection(home)?;
  }
  if let Some(provider) =
    provider.filter(|provider| provider.policy != DaemonDiscoveryPolicy::Preferred)
    && let Some(executable) = provider
      .prepare_for(required)
      .await
      .map_err(ConnectError::PrepareDaemon)?
  {
    return Ok(executable);
  }
  #[cfg(target_os = "macos")]
  {
    default_macos_daemon(current_executable, home, provider)
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = home;
    Ok(sibling_or_path_daemon(current_executable))
  }
}

/// Resolves a bundled, managed, sibling, or PATH daemon without `CTLD_BIN`.
/// On macOS, standalone providers' already verified helper takes priority over
/// nearby desktop bundles. This synchronous resolver never calls the provider
/// or selects unverified shared installations for standalone clients. Desktop
/// clients retain bundle-first discovery. Loose executables remain available.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined or a
/// managed macOS installation is invalid.
pub fn default_daemon_executable() -> Result<PathBuf, ConnectError> {
  if let Some(provider) = DAEMON_PROVIDER
    .get()
    .filter(|provider| provider.policy == DaemonDiscoveryPolicy::Embedded)
  {
    return prepared_daemon(Some(provider)).ok_or_else(|| {
      ConnectError::PrepareDaemon(io::Error::other(
        "the signed development helper must be prepared before synchronous discovery",
      ))
    });
  }
  if let Some(executable) = preferred_daemon(DAEMON_PROVIDER.get()) {
    return Ok(executable);
  }
  #[cfg(unix)]
  if let Some(executable) = selected_bundle_daemon(dirs::home_dir().as_deref())? {
    return Ok(executable);
  }
  let current_executable = env::current_exe().map_err(ConnectError::CurrentExecutable)?;
  #[cfg(target_os = "macos")]
  {
    default_macos_daemon(
      &current_executable,
      dirs::home_dir().as_deref(),
      DAEMON_PROVIDER.get(),
    )
  }
  #[cfg(not(target_os = "macos"))]
  {
    if let Some(executable) = prepared_daemon(DAEMON_PROVIDER.get()) {
      return Ok(executable);
    }
    Ok(sibling_or_path_daemon(&current_executable))
  }
}

fn prepared_daemon(provider: Option<&DaemonProvider>) -> Option<PathBuf> {
  provider
    .and_then(|provider| provider.executable.get())
    .cloned()
}

fn preferred_daemon(provider: Option<&DaemonProvider>) -> Option<PathBuf> {
  prepared_daemon(provider.filter(|provider| provider.policy == DaemonDiscoveryPolicy::Preferred))
}

#[cfg(unix)]
fn selected_bundle_daemon(home: Option<&Path>) -> Result<Option<PathBuf>, ConnectError> {
  let Some(home) = home else {
    return Ok(None);
  };
  ctl_core::bundles::selected_executable_at(
    home,
    "ctld",
    &[
      ("ctld", SUPPORTED_PROTOCOL_VERSIONS),
      ("ctld_lifecycle", lifecycle::SUPPORTED_PROTOCOL_VERSIONS),
      ("ctld_helper", SUPPORTED_HELPER_API_VERSIONS),
    ],
  )
  .map_err(ConnectError::PrepareDaemon)
}

#[cfg(target_os = "macos")]
fn shared_first(provider: Option<&DaemonProvider>) -> bool {
  provider.is_some_and(|provider| {
    matches!(
      provider.policy,
      DaemonDiscoveryPolicy::Shared | DaemonDiscoveryPolicy::Preferred
    )
  })
}

fn sibling_or_path_daemon(current_executable: &Path) -> PathBuf {
  let sibling = current_executable.with_file_name(format!("ctld{}", env::consts::EXE_SUFFIX));
  if sibling.is_file() {
    return sibling;
  }
  PathBuf::from(format!("ctld{}", env::consts::EXE_SUFFIX))
}

#[cfg(target_os = "macos")]
fn default_macos_daemon(
  current_executable: &Path,
  home: Option<&Path>,
  provider: Option<&DaemonProvider>,
) -> Result<PathBuf, ConnectError> {
  if let Some(executable) = preferred_daemon(provider) {
    return Ok(executable);
  }
  if shared_first(provider) {
    validate_managed_selection(home)?;
    if let Some(executable) = prepared_daemon(provider) {
      return Ok(executable);
    }
  }
  if let Some(helper) = bundled_macos_daemon(current_executable)
    && helper.is_file()
  {
    return Ok(helper);
  }
  if shared_first(provider) {
    return Ok(sibling_or_path_daemon(current_executable));
  }
  validate_managed_selection(home)?;
  if let Some(executable) = prepared_daemon(provider) {
    return Ok(executable);
  }
  if let Some(home) = home
    && let Some(helper) =
      managed::resolve_executable(home).map_err(|source| ConnectError::StartDaemon {
        executable: managed::executable(home),
        source,
      })?
  {
    return Ok(helper);
  }
  Ok(sibling_or_path_daemon(current_executable))
}

#[cfg(target_os = "macos")]
fn validate_managed_selection(home: Option<&Path>) -> Result<(), ConnectError> {
  if let Some(home) = home {
    managed::validate_current_selection(home).map_err(|source| ConnectError::StartDaemon {
      executable: managed::executable(home),
      source,
    })?;
  }
  Ok(())
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
  fn prompt_warnings_are_optional_for_existing_protocol_clients() {
    #[derive(serde::Deserialize)]
    struct LegacyPrompt {
      prompt_id: String,
      kind: PromptKind,
      message: String,
    }

    let legacy = serde_json::json!({
      "type": "prompt", "prompt_id": "one", "kind": "secret", "message": "Passphrase:"
    });
    let prompt: ServerMessage = serde_json::from_value(legacy.clone()).unwrap();
    assert!(matches!(
      &prompt,
      ServerMessage::Prompt { warning: None, .. }
    ));
    assert_eq!(serde_json::to_value(prompt).unwrap(), legacy);

    let mut warned = legacy.clone();
    warned["warning"] = "Keychain access is unavailable.".into();
    let prompt: ServerMessage = serde_json::from_value(warned.clone()).unwrap();
    assert!(
      matches!(&prompt, ServerMessage::Prompt { warning: Some(value), .. }
      if value == "Keychain access is unavailable.")
    );
    assert_eq!(serde_json::to_value(prompt).unwrap(), warned);

    let old: LegacyPrompt = serde_json::from_value(warned).unwrap();
    assert_eq!(old.prompt_id, "one");
    assert!(matches!(old.kind, PromptKind::Secret));
    assert_eq!(old.message, "Passphrase:");
  }

  #[test]
  fn existing_gateway_json_remains_unchanged_and_vpn_references_are_strict() {
    let legacy = serde_json::json!({
      "destination": "bastion",
      "hostname": null,
      "user": null,
      "port": null,
      "identity_file": null,
      "mode": "automatic"
    });
    let gateway: SshGateway = serde_json::from_value(legacy.clone()).unwrap();
    assert!(gateway.has_valid_vpn_configuration());
    assert_eq!(serde_json::to_value(&gateway).unwrap(), legacy);
    let vpn = SshGateway {
      kind: GatewayKind::Vpn,
      vpn: Some(VpnGateway {
        connection_id: "saved-vpn".into(),
        socket_path: std::env::temp_dir().join("test-vpn-owner.sock"),
        expected_remote_id: None,
      }),
      destination: "saved-vpn".into(),
      ..gateway
    };
    assert!(vpn.has_valid_vpn_configuration());
    let json = serde_json::to_value(&vpn).unwrap();
    assert_eq!(json["kind"], "vpn");
    assert_eq!(serde_json::from_value::<SshGateway>(json).unwrap(), vpn);
    for field in 0..10 {
      let mut invalid = vpn.clone();
      match field {
        0 => invalid.kind = GatewayKind::Ssh,
        1 => invalid.vpn = None,
        2 => invalid.hostname = Some("localhost".into()),
        3 => invalid.user = Some("alice".into()),
        4 => invalid.port = Some(1080),
        5 => invalid.identity_file = Some("key".into()),
        6 => invalid.mode = SshGatewayMode::NativeOnly,
        7 => invalid.destination = "another-vpn".into(),
        8 => invalid.vpn.as_mut().unwrap().socket_path = "relative.sock".into(),
        _ => {
          invalid.destination.clear();
          invalid.vpn.as_mut().unwrap().connection_id.clear();
        }
      }
      assert!(!invalid.has_valid_vpn_configuration(), "field {field}");
    }
  }

  #[test]
  fn remote_vpn_owner_keeps_alias_overrides_and_its_exact_preceding_route() {
    let gateway: SshGateway = serde_json::from_value(serde_json::json!({
      "destination": "prior-hop", "hostname": null, "user": "bob", "port": 2220,
      "identity_file": null, "mode": "automatic"
    }))
    .unwrap();
    let mut owner = gateway.clone();
    owner.destination = "saved-alias".into();
    owner.hostname = Some("10.0.0.7".into());
    owner.user = Some("alice".into());
    let vpn = SshGateway {
      kind: GatewayKind::Vpn,
      vpn: Some(VpnGateway {
        connection_id: "work".into(),
        socket_path: std::env::temp_dir().join("unused.sock"),
        expected_remote_id: None,
      }),
      destination: "work".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    };
    let route = [gateway.clone(), owner, vpn];
    let target = vpn_owner_target(&route, 2).unwrap();
    assert_eq!(target.destination, "saved-alias");
    assert_eq!(target.ssh_config_alias.as_deref(), Some("saved-alias"));
    assert_eq!(target.hostname.as_deref(), Some("10.0.0.7"));
    assert_eq!(target.user.as_deref(), Some("alice"));
    assert_eq!(target.port, Some(2220));
    assert_eq!(target.gateways, [gateway]);
    assert!(!target.uses_ssh_config_master());
    assert_eq!(target.use_ssh_config_master, Some(false));
  }

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

  fn protocol_metadata(version: ProtocolVersion, supported: &[ProtocolVersion]) -> String {
    serde_json::to_string(&ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: vec![ProtocolInfo::new("ctld", version.build, version, supported)],
    })
    .unwrap()
  }

  #[test]
  fn protocol_probe_requires_valid_published_metadata() {
    let metadata = protocol_metadata(PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS);
    assert_eq!(
      parse_daemon_protocol(metadata.as_bytes()).unwrap().version,
      PROTOCOL_VERSION
    );
    for output in [b"".as_slice(), b"ctld 0.1.0", b"12\n", b"65536", b"\xff"] {
      assert_eq!(
        parse_daemon_protocol(output).unwrap_err().kind(),
        io::ErrorKind::InvalidData
      );
    }
    let mut info: ComponentInfo = serde_json::from_str(&metadata).unwrap();
    info.protocols.push(info.protocols[0].clone());
    assert!(parse_daemon_protocol(&serde_json::to_vec(&info).unwrap()).is_err());
  }

  #[test]
  fn default_endpoint_uses_the_public_protocol_major() {
    let path = default_socket_path();
    #[cfg(unix)]
    assert_eq!(path.file_name().unwrap(), "ctld-v1.sock");
    #[cfg(windows)]
    assert!(path.to_string_lossy().starts_with(r"\\.\pipe\ctld-v1-"));
  }

  #[test]
  fn historical_local_vpn_routes_keep_their_contract_and_wire_shape() {
    let local: SshGateway = serde_json::from_value(serde_json::json!({
      "kind": "vpn",
      "vpn": { "connection_id": "work", "socket_path": "/tmp/owner.sock" },
      "destination": "work", "hostname": null, "user": null,
      "port": null, "identity_file": null, "mode": "automatic"
    }))
    .unwrap();
    assert!(has_valid_gateway_route(std::slice::from_ref(&local)));
    assert!(gateway_route_supported(
      std::slice::from_ref(&local),
      CONTRACT_V1_0_12
    ));
    assert!(gateway_route_supported(
      std::slice::from_ref(&local),
      CONTRACT_V1_1_13
    ));
    let wire = serde_json::to_value(&local).unwrap();
    assert!(wire["vpn"].get("expected_remote_id").is_none());
    let mut ssh = local.clone();
    ssh.kind = GatewayKind::Ssh;
    ssh.vpn = None;
    ssh.destination = "jump".into();
    let remote = [ssh, local];
    assert!(has_valid_gateway_route(&remote));
    assert!(!gateway_route_supported(&remote, CONTRACT_V1_0_12));
    assert!(gateway_route_supported(&remote, CONTRACT_V1_1_13));
    assert_eq!(
      protocol_offer().negotiate(&[CONTRACT_V1_0_12]),
      Some(CONTRACT_V1_0_12)
    );
  }

  #[cfg(unix)]
  pub(crate) static SUBPROCESS_FIXTURE_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

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
      let execution_guard = SUBPROCESS_FIXTURE_LOCK.lock().await;
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
        format!("#!/bin/sh\nset -eu\n[ \"$1\" = --component-info ]\n{body}\n"),
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
    let fixture = ProtocolFixture::new(&format!(
      "printf '%s\\n' '{}'",
      protocol_metadata(PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS)
    ))
    .await;
    check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn remote_proxy_routes_require_the_explicit_helper_contract() {
    for (version, supported, accepted) in [
      (
        HELPER_API_CONTRACT_V1_0_1,
        &[HELPER_API_CONTRACT_V1_0_1][..],
        false,
      ),
      (
        HELPER_API_CONTRACT_V1_1_2,
        &[HELPER_API_CONTRACT_V1_0_1, HELPER_API_CONTRACT_V1_1_2][..],
        true,
      ),
      (HELPER_API_VERSION, SUPPORTED_HELPER_API_VERSIONS, true),
    ] {
      let metadata = serde_json::to_string(&ComponentInfo {
        build: ctl_core::component::build_info(),
        protocols: vec![ProtocolInfo::new(
          "ctld_helper",
          version.build,
          version,
          supported,
        )],
      })
      .unwrap();
      let fixture = ProtocolFixture::new(&format!("printf '%s\\n' '{metadata}'")).await;
      assert_eq!(
        check_remote_proxy_helper(&fixture.executable).await.is_ok(),
        accepted
      );
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn new_clients_can_still_select_the_original_broker_contract() {
    let fixture = ProtocolFixture::new(&format!(
      "printf '%s\\n' '{}'",
      protocol_metadata(CONTRACT_V1_0_12, &[CONTRACT_V1_0_12])
    ))
    .await;
    check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_accepts_a_newer_helper_advertising_the_required_contract() {
    let newer = ProtocolVersion::new(1, PROTOCOL_VERSION.minor + 1, PROTOCOL_BUILD + 1);
    let mut supported = SUPPORTED_PROTOCOL_VERSIONS.to_vec();
    supported.push(newer);
    let fixture = ProtocolFixture::new(&format!(
      "printf '%s\\n' '{}'",
      protocol_metadata(newer, &supported)
    ))
    .await;
    check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_ignores_inherited_askpass_modes() {
    let fixture = ProtocolFixture::new(&format!(
      "[ \"${{CTLD_ASKPASS:-}}\" != 1 ]\n[ \"${{CTLD_IDENTITY_ASKPASS:-}}\" != 1 ]\nprintf '%s\\n' '{}'", protocol_metadata(PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS)
    ))
    .await;
    let mut child = tokio::process::Command::new(env::current_exe().unwrap());
    child
      .args([
        "--exact",
        "tests::protocol_probe_environment_child",
        "--nocapture",
      ])
      .env("CTLD_PROTOCOL_TEST_EXECUTABLE", &fixture.executable)
      .env("CTLD_ASKPASS", "1")
      .env("CTLD_IDENTITY_ASKPASS", "1")
      .kill_on_drop(true);
    let output = timeout(Duration::from_secs(5), child.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_environment_child() {
    let Some(executable) = env::var_os("CTLD_PROTOCOL_TEST_EXECUTABLE") else {
      return;
    };
    assert_eq!(env::var("CTLD_ASKPASS").unwrap(), "1");
    assert_eq!(env::var("CTLD_IDENTITY_ASKPASS").unwrap(), "1");
    check_daemon_protocol(&PathBuf::from(executable), PROTOCOL_QUERY_TIMEOUT)
      .await
      .unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn protocol_probe_rejects_outdated_helper_with_selected_path() {
    let previous_version = ProtocolVersion::new(1, 0, 11);
    let fixture = ProtocolFixture::new(&format!(
      "printf '%s\\n' '{}'",
      protocol_metadata(previous_version, &[previous_version])
    ))
    .await;
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
        error.to_string().contains("--component-info"),
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
  async fn oversized_metadata_is_rejected_without_waiting_for_query_timeout() {
    let fixture = ProtocolFixture::new("exec /usr/bin/yes metadata").await;
    let error = timeout(
      Duration::from_secs(1),
      check_daemon_protocol(&fixture.executable, PROTOCOL_QUERY_TIMEOUT),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(
      matches!(error, ConnectError::CheckDaemonProtocol { source, .. } if source.kind() == io::ErrorKind::InvalidData && source.to_string().contains("oversized"))
    );
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn pinned_daemon_bootstrap_ignores_environment_and_preserves_existing_owners() {
    use std::os::unix::fs::PermissionsExt as _;

    for mode in ["bootstrap", "symlink", "path", "existing"] {
      let fixture = ProtocolFixture::new("exit 1").await;
      // Existence signals readiness; publish only after the whole invocation
      // is recorded so the child cannot observe a partially written line.
      std::fs::write(
        &fixture.executable,
        format!(
          "#!/bin/sh\nset -eu\nif [ \"$1\" = --component-info ]; then\n  if [ -n \"${{CTLD_PINNED_TEST_LINK:-}}\" ]; then\n    /bin/ln -s \"$CTLD_PINNED_TEST_NEXT\" \"$CTLD_PINNED_TEST_LINK.next\"\n    /bin/mv \"$CTLD_PINNED_TEST_LINK.next\" \"$CTLD_PINNED_TEST_LINK\"\n  fi\n  printf '%s\\n' '{}'\n  exit 0\nfi\nprintf '%s\\n' \"$1\" \"$2\" \"$3\" \"$CTLD_SOCKET_PATH\" \"$CTLD_BIN\" > \"$CTLD_PINNED_TEST_MARKER.tmp\"\n/bin/mv \"$CTLD_PINNED_TEST_MARKER.tmp\" \"$CTLD_PINNED_TEST_MARKER\"\n", protocol_metadata(PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS)
        ),
      )
      .unwrap();
      let staged = fixture.directory.join("staged-ctld");
      std::os::unix::fs::symlink(&fixture.executable, &staged).unwrap();
      let next = fixture.directory.join("next-ctld");
      std::fs::copy(&fixture.executable, &next).unwrap();
      let blocked = fixture.directory.join("non-executable");
      std::fs::create_dir(&blocked).unwrap();
      std::fs::write(blocked.join("ctld"), "not executable").unwrap();
      std::fs::set_permissions(blocked.join("ctld"), std::fs::Permissions::from_mode(0o600))
        .unwrap();
      let search_path = env::join_paths([&blocked, &fixture.directory]).unwrap();
      let mut child = tokio::process::Command::new(env::current_exe().unwrap());
      child
        .args(["--exact", "tests::pinned_daemon_child", "--nocapture"])
        .env("CTLD_PINNED_TEST_MODE", mode)
        .env("CTLD_PINNED_TEST_EXECUTABLE", &fixture.executable)
        .env("CTLD_PINNED_TEST_MARKER", fixture.directory.join("started"))
        .env(
          DAEMON_EXECUTABLE_ENV,
          fixture.directory.join("missing-override"),
        );
      if mode == "symlink" {
        child
          .env("CTLD_PINNED_TEST_LINK", &staged)
          .env("CTLD_PINNED_TEST_NEXT", &next);
      } else if mode == "path" {
        child.env("PATH", search_path);
      }
      let output = timeout(Duration::from_secs(5), child.kill_on_drop(true).output())
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
    let selected = match mode.as_str() {
      "symlink" => PathBuf::from(env::var_os("CTLD_PINNED_TEST_LINK").unwrap()),
      "path" => PathBuf::from("ctld"),
      _ => executable.clone(),
    };
    let server = async {
      while !marker.exists() {
        sleep(Duration::from_millis(5)).await;
      }
      let owner = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
      owner.accept().await.unwrap();
    };
    let (connection, ()) = tokio::join!(
      connect_or_start_daemon_at_with_executable(&endpoint.0, Some(&selected)),
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
        executable.canonicalize().unwrap().to_str().unwrap(),
      ]
    );
    if mode == "symlink" {
      assert_ne!(
        selected.canonicalize().unwrap(),
        executable.canonicalize().unwrap()
      );
    }
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn locates_ctld_in_the_macos_helper_bundle() {
    let executable = Path::new("/Applications/ctmux.app/Contents/MacOS/ctmux");
    assert_eq!(
      bundled_macos_daemon(executable),
      Some(PathBuf::from(
        "/Applications/ctmux.app/Contents/Helpers/ctld.app/Contents/MacOS/ctld"
      ))
    );
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn signed_managed_helper_has_priority_over_loose_daemons_and_invalid_selection_fails() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
      fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
      }
    }
    let fixture = Fixture(env::temp_dir().join(format!(
      "ctld-discovery-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    )));
    std::fs::create_dir(&fixture.0).unwrap();
    std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let current_executable = fixture.0.join("ctl");
    assert_eq!(
      default_macos_daemon(&current_executable, Some(&fixture.0), None).unwrap(),
      PathBuf::from("ctld")
    );
    let sibling = fixture.0.join("ctld");
    std::fs::write(&sibling, "source-built helper").unwrap();
    assert_eq!(
      default_macos_daemon(&current_executable, Some(&fixture.0), None).unwrap(),
      sibling
    );

    let directory = managed::ensure_component_directory(&fixture.0).unwrap();
    let selection = "versions/0.1.0-aarch64-apple-darwin";
    let contents = directory.join(selection).join("ctld.app/Contents");
    std::fs::create_dir_all(contents.join("MacOS")).unwrap();
    std::fs::create_dir(contents.join("_CodeSignature")).unwrap();
    for resource in [
      "Info.plist",
      "embedded.provisionprofile",
      "_CodeSignature/CodeResources",
      "CodeResources",
      "MacOS/ctld",
    ] {
      std::fs::write(contents.join(resource), "signed helper").unwrap();
    }
    let managed_helper = contents.join("MacOS/ctld");
    std::fs::set_permissions(&managed_helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink(selection, directory.join("current")).unwrap();
    assert_eq!(
      default_macos_daemon(&current_executable, Some(&fixture.0), None).unwrap(),
      managed_helper.canonicalize().unwrap()
    );
    std::fs::remove_file(directory.join("current")).unwrap();
    symlink("../outside", directory.join("current")).unwrap();
    assert!(matches!(
      default_macos_daemon(&current_executable, Some(&fixture.0), None),
      Err(ConnectError::StartDaemon { .. })
    ));

    let desktop_executable = fixture.0.join("ctmux.app/Contents/MacOS/ctmux");
    let bundled = bundled_macos_daemon(&desktop_executable).unwrap();
    std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
    std::fs::create_dir_all(desktop_executable.parent().unwrap()).unwrap();
    std::fs::write(desktop_executable.with_file_name("ctld"), "loose helper").unwrap();
    std::fs::write(&bundled, "desktop helper").unwrap();
    assert_eq!(
      default_macos_daemon(&desktop_executable, Some(&fixture.0), None).unwrap(),
      bundled
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

#[cfg(all(test, unix))]
mod provider_tests;
