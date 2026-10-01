//! Shared local VPN client for the CLI and desktop application.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
  ClientMessage, ConnectError, ServerMessage, VpnConnection, VpnProvider, VpnSnapshot, VpnState,
  VpnStatus,
};

/// Selects this client's daemon; shared containers are discovered by that daemon.
#[must_use]
pub fn socket_path() -> PathBuf {
  select_socket_path(
    std::env::var_os("CTLD_VPN_SOCKET_PATH").map(PathBuf::from),
    crate::socket_path(),
  )
}

fn select_socket_path(vpn_override: Option<PathBuf>, inherited: PathBuf) -> PathBuf {
  vpn_override.unwrap_or(inherited)
}

/// A VPN client pinned to one daemon endpoint for every operation.
#[derive(Clone, Debug)]
pub struct Client {
  socket_path: PathBuf,
  daemon_executable: Option<PathBuf>,
}

impl Client {
  #[must_use]
  pub fn new(socket_path: PathBuf) -> Self {
    Self {
      socket_path,
      daemon_executable: None,
    }
  }

  /// Selects the helper used only when this endpoint has no running daemon.
  #[must_use]
  pub fn with_daemon_executable(mut self, executable: PathBuf) -> Self {
    self.daemon_executable = Some(executable);
    self
  }

  /// Starts a VPN from an existing private env file, starting ctld if necessary.
  ///
  /// # Errors
  /// Returns connection, protocol, configuration, or daemon startup failures.
  pub async fn start(&self, env_file: PathBuf) -> Result<VpnStatus, VpnError> {
    self
      .request(
        ClientMessage::StartVpn {
          env_file: std::path::absolute(env_file).map_err(VpnError::EnvFile)?,
        },
        true,
        Duration::from_secs(100),
      )
      .await
      .map(|response| response.status)
  }

  /// Starts a saved connection, starting ctld on the selected endpoint if needed.
  ///
  /// # Errors
  /// Returns connection, protocol, configuration, or daemon startup failures.
  pub async fn start_connection(&self, connection: VpnConnection) -> Result<VpnStatus, VpnError> {
    connection.validate().map_err(VpnError::InvalidConnection)?;
    let tailscale = connection.provider() == VpnProvider::Tailscale;
    if tailscale {
      let capabilities = self
        .request(ClientMessage::VpnStatus, true, Duration::from_secs(15))
        .await?
        .snapshot();
      if !capabilities
        .supported_providers
        .contains(&VpnProvider::Tailscale)
      {
        return Err(VpnError::TailscaleUnsupported);
      }
    }
    self
      .request(
        ClientMessage::StartVpnConnection { connection },
        true,
        Duration::from_secs(if tailscale { 150 } else { 100 }),
      )
      .await
      .map(|response| response.status)
  }

  /// Ensures the selected owner can safely clean up a cancelled Tailscale setup.
  ///
  /// # Errors
  /// Returns an update hint for older owners, or connection/startup errors.
  pub async fn ensure_tailscale_enrollment_supported(&self) -> Result<(), VpnError> {
    let snapshot = self
      .request(ClientMessage::VpnStatus, true, Duration::from_secs(15))
      .await?
      .snapshot();
    if snapshot.supports_tailscale_enrollment
      && snapshot
        .supported_providers
        .contains(&VpnProvider::Tailscale)
    {
      Ok(())
    } else {
      Err(VpnError::Daemon {
        code: "vpn_enrollment_unsupported".into(),
        message: "Update and restart ctld to sign in before saving a Tailscale connection".into(),
      })
    }
  }

  /// Removes a stopped Tailscale identity belonging to this local owner.
  ///
  /// # Errors
  /// Returns an error for active/in-use identities, old owners, or engine errors.
  pub async fn forget_tailscale_identity(&self, connection_id: &str) -> Result<(), VpnError> {
    if connection_id.is_empty()
      || connection_id.len() > 128
      || connection_id
        .chars()
        .any(|c| c.is_control() || c.is_whitespace())
    {
      return Err(VpnError::InvalidConnection(
        "Invalid Tailscale connection ID".into(),
      ));
    }
    self.ensure_tailscale_enrollment_supported().await?;
    self
      .request(
        ClientMessage::ForgetTailscaleIdentity {
          connection_id: connection_id.into(),
        },
        true,
        Duration::from_secs(30),
      )
      .await
      .map(|_| ())
  }

  /// Reads VPN status without starting a daemon or searching other endpoints.
  ///
  /// # Errors
  /// Returns an error for an inaccessible daemon or invalid/late response.
  pub async fn status(&self) -> Result<VpnStatus, VpnError> {
    self
      .request(ClientMessage::VpnStatus, false, Duration::from_secs(5))
      .await
      .map(|response| response.status)
  }

  /// Releases this daemon's VPN interest without starting or stopping ctld itself.
  /// A shared container can remain available to other daemons until its timeout.
  ///
  /// # Errors
  /// Returns an error for an inaccessible daemon or invalid/late response.
  pub async fn stop(&self) -> Result<VpnStatus, VpnError> {
    self
      .request(ClientMessage::StopVpn, false, Duration::from_secs(15))
      .await
      .map(|response| response.status)
  }

  /// Lists this daemon's connections and discovered shared containers without starting it.
  ///
  /// # Errors
  /// Returns connection, protocol, or timeout failures.
  pub async fn list(&self) -> Result<VpnSnapshot, VpnError> {
    self
      .request(ClientMessage::VpnStatus, false, Duration::from_secs(5))
      .await
      .map(Response::snapshot)
  }

  /// Releases exactly one local connection. Legacy owners cannot stop by ID atomically,
  /// so an active legacy connection requires an update or an explicit `stop()`.
  ///
  /// # Errors
  /// Returns unsupported targeting, a target mismatch, connection, protocol,
  /// or timeout failure.
  pub async fn stop_id(&self, vpn_id: &str) -> Result<VpnStatus, VpnError> {
    let snapshot = self.list().await?;
    if snapshot.supports_multiple {
      return self
        .request(
          ClientMessage::StopVpnById {
            vpn_id: vpn_id.to_owned(),
          },
          false,
          Duration::from_secs(15),
        )
        .await
        .map(|response| response.status);
    }
    if snapshot.connections.is_empty() {
      return Ok(VpnStatus::default());
    }
    if snapshot.connections.len() != 1 || snapshot.connections[0].vpn_id.as_deref() != Some(vpn_id)
    {
      return Err(VpnError::Daemon {
        code: "vpn_not_found".into(),
        message: "The selected VPN is not owned by this daemon".into(),
      });
    }
    Err(VpnError::Daemon {
      code: "vpn_targeted_stop_unsupported".into(),
      message: "This daemon cannot safely disconnect a VPN by ID; update ctld, or use `ctl vpn stop` to disconnect its current VPN".into(),
    })
  }

  async fn request(
    &self,
    message: ClientMessage,
    start_daemon: bool,
    deadline: Duration,
  ) -> Result<Response, VpnError> {
    if !cfg!(unix) {
      return Err(VpnError::UnsupportedPlatform);
    }
    tokio::time::timeout(deadline, async {
      let connected = if start_daemon {
        crate::connect_or_start_daemon_at_with_executable(
          &self.socket_path,
          self.daemon_executable.as_deref(),
        )
        .await
      } else {
        crate::connect_existing_at(&self.socket_path).await
      };
      let mut stream = match connected {
        Ok(stream) => stream,
        Err(ConnectError::Connect(error))
          if !start_daemon
            && matches!(
              error.kind(),
              io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
          return Ok(Response {
            status: VpnStatus::default(),
            snapshot: Some(VpnSnapshot {
              discovery_warnings: vec!["The selected ctld is not running; VPN container inventory is unavailable. Connect a saved VPN or start ctld to inspect it.".into()],
              ..VpnSnapshot::default()
            }),
          });
        }
        Err(error) => return Err(error.into()),
      };
      exchange(&mut stream, &message).await
    })
    .await
    .map_err(|_| VpnError::Timeout)?
  }
}

impl Default for Client {
  fn default() -> Self {
    Self::new(socket_path())
  }
}

/// Starts a VPN through the environment-selected daemon, starting it if needed.
///
/// # Errors
/// Returns connection, protocol, configuration, or daemon startup failures.
pub async fn start(env_file: PathBuf) -> Result<VpnStatus, VpnError> {
  Client::default().start(env_file).await
}

/// Starts saved settings through the environment-selected daemon.
///
/// # Errors
/// Returns connection, protocol, configuration, or daemon startup failures.
pub async fn start_connection(connection: VpnConnection) -> Result<VpnStatus, VpnError> {
  Client::default().start_connection(connection).await
}

/// Reads VPN status from the environment-selected daemon without starting it.
///
/// # Errors
/// Returns an error for an inaccessible daemon or invalid/late response.
pub async fn status() -> Result<VpnStatus, VpnError> {
  Client::default().status().await
}

/// Releases the environment-selected daemon's VPN interest without starting it.
///
/// # Errors
/// Returns an error for an inaccessible daemon or invalid/late response.
pub async fn stop() -> Result<VpnStatus, VpnError> {
  Client::default().stop().await
}

/// Lists connections through the environment-selected daemon.
///
/// # Errors
/// Returns connection, protocol, or timeout failures.
pub async fn list() -> Result<VpnSnapshot, VpnError> {
  Client::default().list().await
}

/// Stops a selected connection through the environment-selected daemon.
/// Active legacy owners require an update or an explicit untargeted `stop()`.
///
/// # Errors
/// Returns unsupported targeting, a target mismatch, connection, protocol,
/// or timeout failure.
pub async fn stop_id(vpn_id: &str) -> Result<VpnStatus, VpnError> {
  Client::default().stop_id(vpn_id).await
}

#[derive(Debug)]
struct Response {
  status: VpnStatus,
  snapshot: Option<VpnSnapshot>,
}

impl Response {
  fn snapshot(self) -> VpnSnapshot {
    self.snapshot.unwrap_or_else(|| {
      let status = normalize_status(self.status);
      let active = status.running || status.state != VpnState::Stopped;
      VpnSnapshot {
        connections: if active { vec![status] } else { Vec::new() },
        supports_multiple: false,
        supported_providers: vec![VpnProvider::Openconnect],
        supports_tailscale_enrollment: false,
        discovery_warnings: Vec::new(),
      }
    })
  }
}

/// Only browser sign-in links issued by the supported Tailscale control plane.
#[must_use]
pub fn is_tailscale_auth_url(value: &str) -> bool {
  if value.len() > 2048 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
    return false;
  }
  let Ok(url) = url::Url::parse(value) else {
    return false;
  };
  url.scheme() == "https"
    && url.host_str() == Some("login.tailscale.com")
    && url.username().is_empty()
    && url.password().is_none()
    && url.port().is_none()
    && url.query().is_none()
    && url.fragment().is_none()
    && url.path().strip_prefix("/a/").is_some_and(|token| {
      !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

fn normalize_status(mut status: VpnStatus) -> VpnStatus {
  if (status.running || status.state != VpnState::Stopped) && status.vpn_id.is_none() {
    status.vpn_id = Some(
      status
        .connection_id
        .clone()
        .or_else(|| status.container_name.clone())
        .unwrap_or_else(|| "legacy".into()),
    );
  }
  status
}

async fn exchange<S>(stream: &mut S, message: &ClientMessage) -> Result<Response, VpnError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  crate::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: crate::PROTOCOL_VERSION,
    },
  )
  .await?;
  match crate::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version }) => {
      if protocol_version != crate::PROTOCOL_VERSION {
        return Err(VpnError::ProtocolVersionMismatch {
          expected: crate::PROTOCOL_VERSION,
          actual: protocol_version,
        });
      }
    }
    Some(ServerMessage::Error { code, message }) => return Err(VpnError::Daemon { code, message }),
    None => return Err(VpnError::ConnectionClosed),
    Some(_) => return Err(VpnError::UnexpectedResponse),
  }
  crate::write_frame(stream, message).await?;
  match crate::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::VpnStatus { status, snapshot })
      if !matches!(message, ClientMessage::ForgetTailscaleIdentity { .. }) =>
    {
      Ok(Response {
        status: normalize_status(*status),
        snapshot,
      })
    }
    Some(ServerMessage::VpnIdentityForgotten)
      if matches!(message, ClientMessage::ForgetTailscaleIdentity { .. }) =>
    {
      Ok(Response {
        status: VpnStatus::default(),
        snapshot: None,
      })
    }
    Some(ServerMessage::Error { code, message }) => Err(VpnError::Daemon { code, message }),
    None => Err(VpnError::ConnectionClosed),
    Some(_) => Err(VpnError::UnexpectedResponse),
  }
}

#[derive(Debug, thiserror::Error)]
pub enum VpnError {
  #[error("This ctld does not support Tailscale; update and restart ctld")]
  TailscaleUnsupported,
  #[error("could not resolve the VPN settings file: {0}")]
  EnvFile(#[source] io::Error),
  #[error("{0}")]
  InvalidConnection(String),
  #[error(transparent)]
  Connect(#[from] ConnectError),
  #[error(transparent)]
  Codec(#[from] crate::CodecError),
  #[error("ctld closed the VPN request; update ctld to match the client and restart it")]
  ConnectionClosed,
  #[error("ctld returned an unexpected response to the VPN request")]
  UnexpectedResponse,
  #[error(
    "ctld protocol mismatch: client requires {expected}, daemon accepted {actual}; update and restart ctld"
  )]
  ProtocolVersionMismatch { expected: u16, actual: u16 },
  #[error("ctld error {code}: {message}")]
  Daemon { code: String, message: String },
  #[error("VPN request timed out; check its status before trying again")]
  Timeout,
  #[error("VPN management requires macOS or Linux")]
  UnsupportedPlatform,
}

impl VpnError {
  #[must_use]
  pub fn code(&self) -> &str {
    match self {
      Self::TailscaleUnsupported => "vpn_provider_unsupported",
      Self::EnvFile(_) | Self::InvalidConnection(_) => "vpn_invalid_connection",
      Self::Connect(_) => "ctld_connection_failed",
      Self::Codec(_) | Self::UnexpectedResponse => "ctld_protocol_error",
      Self::ConnectionClosed => "ctld_connection_closed",
      Self::ProtocolVersionMismatch { .. } => "ctld_protocol_version_mismatch",
      Self::Daemon { code, .. } => code,
      Self::Timeout => "vpn_timeout",
      Self::UnsupportedPlatform => "vpn_unsupported",
    }
  }
}

#[cfg(test)]
mod tests;
