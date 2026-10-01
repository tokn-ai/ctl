//! Reusable saved connections. SSH config projections never enter this store.

use serde::Deserialize;

use super::repository::{Repository, content_revision};
use super::{WorkspaceHost, WorkspaceSshGateway};
use crate::error::{CommandErrorDto, CommandResult};

pub use ctl_client::hosts::{HostCatalogDocument, HostCatalogSnapshot};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateHostsRequest {
  pub expected_revision: Option<String>,
  pub document: HostCatalogDocument,
}

impl Repository {
  pub(super) fn read_catalog(&self) -> CommandResult<HostCatalogSnapshot> {
    ctl_client::hosts::storage::load(&self.directory.join("hosts.json")).map_err(Into::into)
  }

  pub(super) fn persist_catalog(&self, snapshot: &HostCatalogSnapshot) -> CommandResult<()> {
    ctl_client::hosts::storage::persist_under_lock(&self.directory.join("hosts.json"), snapshot)
      .map_err(Into::into)
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
