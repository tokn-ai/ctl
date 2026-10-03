use super::{HostError, WorkspaceHost, WorkspaceSshGateway, validated_gateway_ids};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCatalogDocument {
  pub schema_version: u32,
  pub hosts: Vec<WorkspaceHost>,
  pub ssh_gateways: Vec<WorkspaceSshGateway>,
}

impl Default for HostCatalogDocument {
  fn default() -> Self {
    Self {
      schema_version: 1,
      hosts: Vec::new(),
      ssh_gateways: Vec::new(),
    }
  }
}

impl HostCatalogDocument {
  /// Validate the saved document before reading or writing it.
  ///
  /// # Errors
  /// Returns an error for an unsupported version or invalid saved settings.
  pub fn validate(&self) -> Result<(), HostError> {
    if self.schema_version != 1 {
      return Err(HostError::new(
        "hosts_version_unsupported",
        "This host catalog was written by another app version. Its file has not been changed.",
      ));
    }
    let invalid = || {
      HostError::new(
        "hosts_invalid",
        "The host catalog contains invalid or duplicate connections.",
      )
    };
    let gateway_ids = validated_gateway_ids(&self.ssh_gateways).ok_or_else(invalid)?;
    let mut host_ids = HashSet::new();
    if self.hosts.len() > 1024
      || self.hosts.iter().any(|host| {
        host.host_id == "local"
          || !host_ids.insert(host.host_id.as_str())
          || !host.is_valid(&gateway_ids)
      })
    {
      return Err(invalid());
    }
    // A proxy can forward traffic but cannot host a managed VPN. Validate the
    // execution context with the actual saved gateway kinds before persisting.
    for host in &self.hosts {
      for method in &host.connection_methods {
        let super::ConnectionTargetDto::Ssh {
          gateway_route,
          vpn_connection_id,
          ..
        } = &method.target
        else {
          return Err(invalid());
        };
        let mut previous = vpn_connection_id
          .as_ref()
          .map(|_| ctl_ipc::GatewayKind::Vpn);
        for step in gateway_route {
          previous = Some(match step {
            super::SshGatewayRouteStepDto::Gateway { gateway_id, .. } => {
              self
                .ssh_gateways
                .iter()
                .find(|gateway| gateway.gateway_id == *gateway_id)
                .ok_or_else(invalid)?
                .kind
            }
            super::SshGatewayRouteStepDto::Vpn { .. } => {
              if previous.is_some_and(|kind| kind != ctl_ipc::GatewayKind::Ssh) {
                return Err(invalid());
              }
              ctl_ipc::GatewayKind::Vpn
            }
          });
        }
      }
    }
    Ok(())
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCatalogSnapshot {
  pub revision: Option<String>,
  pub document: HostCatalogDocument,
}
