//! Reusable saved connections. SSH config projections never enter this store.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};

use super::repository::{
  MAX_WORKSPACE_BYTES, Repository, content_revision, regular_file_or_absent,
};
use super::{WorkspaceHost, WorkspaceSshGateway, validated_gateway_ids};
use crate::error::{CommandErrorDto, CommandResult};

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
  pub(super) fn validate(&self) -> CommandResult<()> {
    if self.schema_version != 1 {
      return Err(CommandErrorDto::new(
        "hosts_version_unsupported",
        "This host catalog was written by another app version. Its file has not been changed.",
      ));
    }
    let invalid = || {
      CommandErrorDto::new(
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
    Ok(())
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCatalogSnapshot {
  pub revision: Option<String>,
  pub document: HostCatalogDocument,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateHostsRequest {
  pub expected_revision: Option<String>,
  pub document: HostCatalogDocument,
}

impl Repository {
  pub(super) fn read_catalog(&self) -> CommandResult<HostCatalogSnapshot> {
    let path = self.directory.join("hosts.json");
    regular_file_or_absent(&path).map_err(io_error)?;
    let file = match File::open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return Ok(HostCatalogSnapshot::default());
      }
      Err(error) => return Err(io_error(error)),
    };
    let mut bytes = Vec::new();
    file
      .take(MAX_WORKSPACE_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(io_error)?;
    if bytes.len() as u64 > MAX_WORKSPACE_BYTES {
      return Err(too_large());
    }
    let mut snapshot: HostCatalogSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
      CommandErrorDto::new(
        "hosts_unreadable",
        format!("Could not read hosts.json; the file has been preserved: {error}"),
      )
    })?;
    snapshot.document.validate()?;
    if snapshot.revision.as_ref().is_none_or(String::is_empty) {
      return Err(CommandErrorDto::new(
        "hosts_invalid",
        "The host catalog has no revision. Its file has not been changed.",
      ));
    }
    snapshot.revision = Some(content_revision(&snapshot.document)?);
    Ok(snapshot)
  }

  pub(super) fn persist_catalog(&self, snapshot: &HostCatalogSnapshot) -> CommandResult<()> {
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(CommandErrorDto::backend)?;
    if bytes.len() as u64 > MAX_WORKSPACE_BYTES {
      return Err(too_large());
    }
    self.write_named("hosts.json", &bytes).map_err(io_error)
  }

  /// Import the complete batch in memory before writing. Conflicts preserve both
  /// stores, and an equal batch is safe to retry after an interrupted migration.
  pub(super) fn import_hosts(
    &self,
    hosts: &[WorkspaceHost],
    gateways: &[WorkspaceSshGateway],
  ) -> CommandResult<()> {
    let mut catalog = self.read_catalog()?;
    let original = catalog.document.clone();
    for host in hosts.iter().filter(|host| host.host_id != "local") {
      if let Some(existing) = catalog
        .document
        .hosts
        .iter()
        .find(|item| item.host_id == host.host_id)
      {
        if existing != host {
          return Err(import_conflict("host", &host.host_id));
        }
      } else {
        catalog.document.hosts.push(host.clone());
      }
    }
    for gateway in gateways {
      if let Some(existing) = catalog
        .document
        .ssh_gateways
        .iter()
        .find(|item| item.gateway_id == gateway.gateway_id)
      {
        if existing != gateway {
          return Err(import_conflict("gateway", &gateway.gateway_id));
        }
      } else {
        catalog.document.ssh_gateways.push(gateway.clone());
      }
    }
    catalog.document.validate()?;
    if catalog.document != original {
      catalog.revision = Some(content_revision(&catalog.document)?);
      self.persist_catalog(&catalog)?;
    }
    Ok(())
  }
}

fn import_conflict(kind: &str, id: &str) -> CommandErrorDto {
  CommandErrorDto::new(
    "hosts_import_conflict",
    format!(
      "The saved {kind} {id:?} differs from the workspace being migrated. Both files have been preserved."
    ),
  )
}

fn too_large() -> CommandErrorDto {
  CommandErrorDto::new(
    "hosts_too_large",
    "The host catalog exceeds its size limit. Its file has not been changed.",
  )
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> CommandErrorDto {
  CommandErrorDto::new(
    "hosts_io_failed",
    format!("Could not access hosts.json: {error}"),
  )
}
