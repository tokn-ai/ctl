use super::{
  HostError, WorkspaceHost, WorkspaceSshGateway, valid_workspace_text, validated_gateway_ids,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCatalogDocument {
  pub schema_version: u32,
  pub hosts: Vec<WorkspaceHost>,
  pub ssh_gateways: Vec<WorkspaceSshGateway>,
  /// Presentation only; local and projected identities need no saved definition.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub host_colors: BTreeMap<String, String>,
}

impl Default for HostCatalogDocument {
  fn default() -> Self {
    Self {
      schema_version: 1,
      hosts: Vec::new(),
      ssh_gateways: Vec::new(),
      host_colors: BTreeMap::new(),
    }
  }
}

impl HostCatalogDocument {
  /// Validate the saved document before reading or writing it.
  ///
  /// # Errors
  /// Returns an error for an unsupported version or invalid saved settings.
  pub fn validate(&self) -> Result<(), HostError> {
    if !matches!(self.schema_version, 1 | 2) {
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
    if (self.schema_version == 1 && !self.host_colors.is_empty())
      || self.host_colors.iter().any(|(host_id, color)| {
        !valid_workspace_text(host_id)
          || color.len() != 7
          || !color.starts_with('#')
          || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
      })
    {
      return Err(invalid());
    }
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
    // Resolve every method before persisting, so linked hosts cannot introduce
    // missing references, cycles, excessive depth, or invalid VPN owners.
    for host in &self.hosts {
      for method in &host.connection_methods {
        super::resolve(self, &host.host_id, Some(&method.method_id))?;
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
