//! Local and remote VPN clients used by CLI commands and connection prerequisites.

use ctl_ipc::{VpnConnection, VpnSnapshot, VpnStatus, vpn};

pub enum Client {
  Local(vpn::Client),
  #[cfg(unix)]
  Remote(ctl_ipc::remote_vpn::Client),
}

impl Client {
  /// Queries the VPN connection inventory from the selected owner.
  ///
  /// # Errors
  /// Returns transport or service errors reported by the selected owner.
  pub async fn list(&self) -> Result<VpnSnapshot, Error> {
    match self {
      Self::Local(client) => Ok(client.list().await?),
      #[cfg(unix)]
      Self::Remote(client) => Ok(client.list().await?),
    }
  }

  /// Starts or resumes the supplied VPN connection on the selected owner.
  ///
  /// # Errors
  /// Returns transport, validation, or service errors reported by the selected owner.
  pub async fn start_connection(&self, connection: VpnConnection) -> Result<VpnStatus, Error> {
    match self {
      Self::Local(client) => Ok(client.start_connection(connection).await?),
      #[cfg(unix)]
      Self::Remote(client) => Ok(client.start_connection(connection).await?),
    }
  }

  /// Requests that the selected owner stop the identified VPN connection.
  ///
  /// # Errors
  /// Returns transport or service errors reported by the selected owner.
  pub async fn stop_id(&self, connection_id: &str) -> Result<VpnStatus, Error> {
    match self {
      Self::Local(client) => Ok(client.stop_id(connection_id).await?),
      #[cfg(unix)]
      Self::Remote(client) => Ok(client.stop_id(connection_id).await?),
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Local(#[from] vpn::VpnError),
  #[error(transparent)]
  Remote(#[from] ctl_ipc::remote_vpn::Error),
}
