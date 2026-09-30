//! Saved host definitions shared by the desktop and command-line clients.

mod catalog;
mod gateways;
mod models;
mod resolver;
mod target;
#[cfg(test)]
mod tests;
mod transport;
mod vpn;
pub use vpn::SavedVpnDocument;

pub use catalog::{HostCatalogDocument, HostCatalogSnapshot};
pub use gateways::{WorkspaceSshGateway, validated_gateway_ids};
pub use models::{WorkspaceConnectionMethod, WorkspaceHost};
pub use resolver::{ResolvedHost, load_catalog, resolve};
pub use target::{ConnectionTargetDto, SshGatewayDto, SshGatewayModeDto, SshGatewayRouteStepDto};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct HostError {
  pub code: String,
  pub message: String,
}

impl HostError {
  pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      code: code.into(),
      message: message.into(),
    }
  }
}

fn valid_workspace_text(text: &str) -> bool {
  !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
}

#[must_use]
pub fn valid_connection_id(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 128
    && !value
      .chars()
      .any(|value| value.is_control() || value.is_whitespace())
}

#[must_use]
pub fn vpn_gateway(connection_id: &str) -> ctld_ipc::SshGateway {
  ctld_ipc::SshGateway {
    kind: ctld_ipc::GatewayKind::Vpn,
    vpn: Some(ctld_ipc::VpnGateway {
      connection_id: connection_id.into(),
      socket_path: ctld_ipc::vpn::socket_path(),
    }),
    destination: connection_id.into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctld_ipc::SshGatewayMode::Automatic,
  }
}
