use std::path::Path;

use super::{ConnectionTargetDto, HostCatalogDocument, HostError, SshGatewayDto};

pub struct ResolvedHost {
  pub host_id: Option<String>,
  pub target: ConnectionTargetDto,
  pub tailscale_node_id: Option<String>,
}

/// Read one atomic catalog snapshot. Invalid saved data must never cause a
/// silent fallback to DNS or a different route.
///
/// # Errors
/// Returns an error for unreadable, oversized, or invalid catalog snapshots.
pub fn load_catalog(path: &Path) -> Result<HostCatalogDocument, HostError> {
  Ok(super::storage::load(path)?.document)
}

/// Saved names and stable IDs take precedence over OpenSSH destinations.
///
/// # Errors
/// Rejects ambiguous hosts, missing methods, and unresolved gateways.
pub fn resolve(
  catalog: &HostCatalogDocument,
  destination: &str,
  method: Option<&str>,
) -> Result<ResolvedHost, HostError> {
  let exact = matching_hosts(catalog, destination);
  let (user, alias) = if exact.is_empty() {
    destination
      .rsplit_once('@')
      .map_or((None, destination), |(user, alias)| (Some(user), alias))
  } else {
    (None, destination)
  };
  let matches = if exact.is_empty() {
    matching_hosts(catalog, alias)
  } else {
    exact
  };
  if matches.len() > 1 {
    return Err(HostError::new(
      "host_ambiguous",
      format!("More than one saved host matches {alias:?}; use its host ID."),
    ));
  }
  let Some(host) = matches.first() else {
    if method.is_some() {
      return Err(HostError::new(
        "host_not_found",
        "--method requires a saved ctl host.",
      ));
    }
    return Ok(ResolvedHost {
      host_id: None,
      target: ConnectionTargetDto::ssh(destination),
      tailscale_node_id: None,
    });
  };
  let selected = method.or(host.preferred_method_id.as_deref());
  let exact_method = host
    .connection_methods
    .iter()
    .any(|item| Some(item.method_id.as_str()) == selected);
  let methods: Vec<_> = host
    .connection_methods
    .iter()
    .filter(|item| {
      Some(item.method_id.as_str()) == selected
        || (!exact_method && method.is_some_and(|value| item.name == value))
    })
    .collect();
  if methods.len() != 1 {
    return Err(HostError::new(
      "method_not_found",
      format!("Choose an unambiguous connection method for {}.", host.name),
    ));
  }
  let method = methods[0];
  let mut target = method.target.clone();
  if let ConnectionTargetDto::Ssh {
    remote_info,
    ssh_config_alias,
    use_ssh_config_master,
    user: account,
    gateway_route,
    gateways,
    ..
  } = &mut target
  {
    *remote_info = host.remote_info.clone().map(Box::new);
    ssh_config_alias.clone_from(&method.ssh_config_alias);
    *use_ssh_config_master = method.use_ssh_config_master;
    if let Some(user) = user {
      // A different account has a different ctl environment.
      *account = Some(user.into());
      *remote_info = None;
    }
    *gateways = gateway_route
      .iter()
      .map(|step| {
        let gateway = catalog
          .ssh_gateways
          .iter()
          .find(|item| item.gateway_id == step.gateway_id)
          .ok_or_else(|| HostError::new("gateway_missing", "A saved gateway is missing."))?;
        Ok(SshGatewayDto {
          kind: gateway.kind,
          gateway_id: gateway.gateway_id.clone(),
          name: gateway.name.clone(),
          destination: gateway.destination.clone(),
          hostname: gateway.hostname.clone(),
          user: gateway.user.clone(),
          port: gateway.port,
          identity_file: gateway.identity_file.clone(),
          remote_info: gateway.remote_info.clone(),
          mode: step.mode,
        })
      })
      .collect::<Result<Box<[_]>, HostError>>()?;
  }
  Ok(ResolvedHost {
    host_id: Some(host.host_id.clone()),
    target,
    tailscale_node_id: method.tailscale_node_id.clone(),
  })
}

fn matching_hosts<'a>(
  catalog: &'a HostCatalogDocument,
  alias: &str,
) -> Vec<&'a super::WorkspaceHost> {
  if let Some(host) = catalog.hosts.iter().find(|host| host.host_id == alias) {
    return vec![host];
  }
  catalog
    .hosts
    .iter()
    .filter(|host| host.name == alias)
    .collect()
}

impl ResolvedHost {
  /// Refresh a saved device binding without falling back to its stale address.
  ///
  /// # Errors
  /// Returns an error when the selected device has disappeared or lacks an address.
  pub fn resolve_tailscale(
    &mut self,
    devices: &[crate::tailscale::TailscaleDevice],
  ) -> Result<(), HostError> {
    let Some(id) = &self.tailscale_node_id else {
      return Ok(());
    };
    let device = devices.iter().find(|device| device.node_id == *id)
      .ok_or_else(|| HostError::new("tailscale_unavailable", "The saved Tailscale device is unavailable. Check that Tailscale is running and signed in to the correct tailnet."))?;
    let address = device
      .addresses
      .iter()
      .find(|address| address.parse::<std::net::IpAddr>().is_ok())
      .ok_or_else(|| {
        HostError::new(
          "tailscale_unavailable",
          "The saved Tailscale device has no usable address.",
        )
      })?;
    if let ConnectionTargetDto::Ssh {
      destination,
      hostname,
      ..
    } = &mut self.target
    {
      destination.clone_from(device.dns_name.as_ref().unwrap_or(address));
      *hostname = Some(address.clone());
    }
    Ok(())
  }
}
