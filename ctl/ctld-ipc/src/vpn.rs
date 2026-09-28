//! Shared local VPN client for the CLI and desktop application.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{ClientMessage, ConnectError, ServerMessage, VpnConnection, VpnStatus};

/// Starts a VPN from an existing private env file, starting ctld if necessary.
///
/// # Errors
/// Returns connection, protocol, configuration, or daemon startup failures.
pub async fn start(env_file: PathBuf) -> Result<VpnStatus, VpnError> {
  request(
    ClientMessage::StartVpn {
      env_file: std::path::absolute(env_file).map_err(VpnError::EnvFile)?,
    },
    true,
    Duration::from_secs(100),
  )
  .await
}

/// Starts a saved connection, starting ctld if necessary.
///
/// # Errors
/// Returns connection, protocol, configuration, or daemon startup failures.
pub async fn start_connection(connection: VpnConnection) -> Result<VpnStatus, VpnError> {
  connection.validate().map_err(VpnError::InvalidConnection)?;
  request(
    ClientMessage::StartVpnConnection { connection },
    true,
    Duration::from_secs(100),
  )
  .await
}

/// Reads VPN status without starting a daemon.
///
/// # Errors
/// Returns an error for an inaccessible daemon or invalid/late response.
pub async fn status() -> Result<VpnStatus, VpnError> {
  request(ClientMessage::VpnStatus, false, Duration::from_secs(5)).await
}

/// Stops the owned VPN without starting or stopping ctld itself.
///
/// # Errors
/// Returns an error for an inaccessible daemon or invalid/late response.
pub async fn stop() -> Result<VpnStatus, VpnError> {
  request(ClientMessage::StopVpn, false, Duration::from_secs(15)).await
}

async fn request(
  message: ClientMessage,
  start_daemon: bool,
  deadline: Duration,
) -> Result<VpnStatus, VpnError> {
  if !cfg!(unix) {
    return Err(VpnError::UnsupportedPlatform);
  }
  tokio::time::timeout(deadline, async {
    let connected = if start_daemon {
      crate::connect_or_start_daemon().await
    } else {
      crate::connect_existing().await
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
        return Ok(VpnStatus::default());
      }
      Err(error) => return Err(error.into()),
    };
    exchange(&mut stream, &message).await
  })
  .await
  .map_err(|_| VpnError::Timeout)?
}

async fn exchange<S>(stream: &mut S, message: &ClientMessage) -> Result<VpnStatus, VpnError>
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
    Some(ServerMessage::VpnStatus { status }) => Ok(status),
    Some(ServerMessage::Error { code, message }) => Err(VpnError::Daemon { code, message }),
    None => Err(VpnError::ConnectionClosed),
    Some(_) => Err(VpnError::UnexpectedResponse),
  }
}

#[derive(Debug, thiserror::Error)]
pub enum VpnError {
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
