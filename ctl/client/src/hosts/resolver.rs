use std::path::Path;

use super::{ConnectionTargetDto, HostCatalogDocument, HostError};

pub struct GatewayTailscaleBinding {
  pub gateway_index: usize,
  pub node_id: String,
}

pub struct ResolvedHost {
  pub host_id: Option<String>,
  pub target: ConnectionTargetDto,
  pub tailscale_node_id: Option<String>,
  pub gateway_tailscale_bindings: Vec<GatewayTailscaleBinding>,
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
      gateway_tailscale_bindings: Vec::new(),
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
  let route = super::route::resolve(catalog, host, method)?;
  let mut target = method.target.clone();
  if let ConnectionTargetDto::Ssh {
    remote_info,
    ssh_config_alias,
    use_ssh_config_master,
    user: account,
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
    *gateways = route.gateways;
  }
  target.to_ssh_target()?;
  Ok(ResolvedHost {
    host_id: Some(host.host_id.clone()),
    target,
    tailscale_node_id: method.tailscale_node_id.clone(),
    gateway_tailscale_bindings: route.tailscale_bindings,
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
  /// Device discovery is needed for every bound endpoint in the expanded route.
  #[must_use]
  pub fn requires_tailscale(&self) -> bool {
    self.tailscale_node_id.is_some() || !self.gateway_tailscale_bindings.is_empty()
  }

  /// Refresh a saved device binding without falling back to its stale address.
  ///
  /// # Errors
  /// Returns an error when the selected device has disappeared or lacks an address.
  pub fn resolve_tailscale(
    &mut self,
    devices: &[crate::tailscale::TailscaleDevice],
  ) -> Result<(), HostError> {
    if !self.requires_tailscale() {
      return Ok(());
    }
    // Resolve all devices before mutating any endpoint; a missing hop must not
    // leave a partly refreshed route that could be used by a caller.
    let endpoint = self
      .tailscale_node_id
      .as_deref()
      .map(|id| tailscale_endpoint(devices, id))
      .transpose()?;
    let gateway_endpoints = self
      .gateway_tailscale_bindings
      .iter()
      .map(|binding| {
        tailscale_endpoint(devices, &binding.node_id)
          .map(|endpoint| (binding.gateway_index, endpoint))
      })
      .collect::<Result<Vec<_>, _>>()?;
    if let ConnectionTargetDto::Ssh {
      destination,
      hostname,
      gateways,
      ..
    } = &mut self.target
    {
      if let Some((name, address)) = endpoint {
        *destination = name;
        *hostname = Some(address);
      }
      for (index, (name, address)) in gateway_endpoints {
        let gateway = gateways.get_mut(index).ok_or_else(|| {
          HostError::new(
            "host_hop_invalid",
            "A Tailscale hop is missing from the resolved route.",
          )
        })?;
        gateway.destination = name;
        gateway.hostname = Some(address);
      }
    }
    Ok(())
  }
}

fn tailscale_endpoint(
  devices: &[crate::tailscale::TailscaleDevice],
  id: &str,
) -> Result<(String, String), HostError> {
  let device = devices.iter().find(|device| device.node_id == id)
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
  Ok((
    device.dns_name.as_ref().unwrap_or(address).clone(),
    address.clone(),
  ))
}
