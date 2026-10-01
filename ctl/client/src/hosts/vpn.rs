use super::HostError;
use ctl_ipc::{VpnConnection, VpnProvider};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
const MAX_CONNECTIONS: usize = 128;
fn error(code: &str, message: impl Into<String>) -> HostError {
  HostError::new(code, message)
}
fn invalid(message: impl Into<String>) -> HostError {
  error("vpn_connections_invalid", message)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedVpnDocument {
  pub schema_version: u32,
  pub connections: Vec<VpnConnection>,
}

impl Default for SavedVpnDocument {
  fn default() -> Self {
    Self {
      schema_version: 2,
      connections: Vec::new(),
    }
  }
}

impl SavedVpnDocument {
  /// Validate the saved document before reading or writing it.
  ///
  /// # Errors
  /// Returns an error for an unsupported version or invalid saved settings.
  pub fn validate(&self) -> Result<(), HostError> {
    if !matches!(self.schema_version, 1 | 2) {
      return Err(error(
        "vpn_version_unsupported",
        "This VPN settings version is not supported.",
      ));
    }
    if self.connections.len() > MAX_CONNECTIONS {
      return Err(invalid("Too many saved VPN connections."));
    }
    let mut ids = HashSet::new();
    for connection in &self.connections {
      if self.schema_version == 1 && connection.provider() != VpnProvider::Openconnect {
        return Err(invalid(
          "Tailscale connections require VPN settings version 2.",
        ));
      }
      connection.validate().map_err(invalid)?;
      if !ids.insert(&connection.connection_id) {
        return Err(invalid("VPN connection IDs must be unique."));
      }
    }
    Ok(())
  }
}
