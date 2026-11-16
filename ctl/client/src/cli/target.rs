use crate::hosts::{self, ConnectionTargetDto, HostError, ResolvedHost};
use std::path::PathBuf;

#[cfg(unix)]
use super::ssh_broker;
use super::{RemotePlatform, vpn_profiles, vpn_runtime};

/// Applies the explicit platform override to an SSH target.
pub fn apply_remote_platform(target: &mut ConnectionTargetDto, selection: Option<RemotePlatform>) {
  if let Some(platform) = selection
    && let ConnectionTargetDto::Ssh {
      remote_platform, ..
    } = target
  {
    *remote_platform = Some(platform.into());
  }
}

/// Resolves a saved host, explicit SSH destination, or the default local target.
///
/// # Errors
/// Returns an error for conflicting arguments, invalid targets, or an unavailable
/// host catalog, connection method, or Tailscale endpoint.
pub async fn resolve(
  host: Option<&str>,
  destination: Option<&str>,
  method: Option<&str>,
) -> Result<ResolvedHost, HostError> {
  if let Some(destination) = destination {
    if host.is_some() || method.is_some() {
      return Err(HostError::new(
        "target_conflict",
        "--destination cannot be combined with --host or --method.",
      ));
    }
    return temporary_destination(destination);
  }
  let Some(host) = host else {
    if method.is_some() {
      return Err(HostError::new("host_required", "--method requires a host."));
    }
    return Ok(ResolvedHost {
      host_id: None,
      target: ConnectionTargetDto::Local,
      tailscale_node_id: None,
      gateway_tailscale_bindings: Vec::new(),
    });
  };
  let path = catalog_path()?;
  let resolved = resolve_saved(&hosts::load_catalog(&path)?, host, method)?;
  refresh_tailscale(resolved).await
}

fn resolve_saved(
  catalog: &hosts::HostCatalogDocument,
  host: &str,
  method: Option<&str>,
) -> Result<ResolvedHost, HostError> {
  if !catalog
    .hosts
    .iter()
    .any(|saved| saved.host_id == host || saved.name == host)
  {
    return Err(HostError::new(
      "host_not_found",
      format!(
        "No saved ctl host matches {host:?}; use --destination for an SSH address or config alias."
      ),
    ));
  }
  hosts::resolve(catalog, host, method)
}

/// Compatibility commands retain saved-host-first positional lookup.
///
/// # Errors
/// Returns an error if the catalog or selected target cannot be resolved.
pub async fn resolve_openssh(
  destination: &str,
  method: Option<&str>,
) -> Result<ResolvedHost, HostError> {
  let path = catalog_path()?;
  let resolved = hosts::resolve(&hosts::load_catalog(&path)?, destination, method)?;
  refresh_tailscale(resolved).await
}

async fn refresh_tailscale(mut resolved: ResolvedHost) -> Result<ResolvedHost, HostError> {
  if resolved.requires_tailscale() {
    resolved.resolve_tailscale(&crate::tailscale::discover_devices().await.devices)?;
  }
  Ok(resolved)
}

fn temporary_destination(destination: &str) -> Result<ResolvedHost, HostError> {
  let mut target = ConnectionTargetDto::ssh(destination);
  if target.to_ssh_target().is_err()
    || destination.starts_with('!')
    || destination.contains(['*', '?'])
  {
    return Err(HostError::new(
      "invalid_ssh_target",
      "Use a valid SSH destination or config alias with --destination.",
    ));
  }
  if let ConnectionTargetDto::Ssh {
    ssh_config_alias, ..
  } = &mut target
  {
    // Let OpenSSH resolve the original destination and its sharing policy.
    // Never copy endpoint overrides or pinned identities from the host catalog.
    *ssh_config_alias = Some(destination.into());
  }
  Ok(ResolvedHost {
    host_id: None,
    target,
    tailscale_node_id: None,
    gateway_tailscale_bindings: Vec::new(),
  })
}

/// Returns the configured host catalog path.
///
/// # Errors
/// Returns an error when there is no override and the home directory is unavailable.
pub fn catalog_path() -> Result<PathBuf, HostError> {
  std::env::var_os("CTL_HOSTS_PATH")
    .map_or_else(
      || {
        ctl_core::paths::config_directory()
          .ok()
          .map(|directory| directory.join("hosts.json"))
      },
      |path| Some(PathBuf::from(path)),
    )
    .ok_or_else(|| HostError::new("home_unavailable", "Could not find the home directory."))
}

/// Starts any VPN prerequisites for the target with terminal interaction enabled.
///
/// # Errors
/// Returns an error if the route, a required saved profile, or a required VPN
/// connection is unavailable.
pub async fn ensure_vpn(target: &ConnectionTargetDto) -> Result<(), Error> {
  ensure_vpn_with_interaction(target, true).await
}

/// Starts VPN prerequisites using the requested terminal interaction policy.
///
/// # Errors
/// Returns an error if a route or required profile is unavailable, or if a
/// required connection cannot start under the selected interaction policy.
pub async fn ensure_vpn_with_interaction(
  target: &ConnectionTargetDto,
  interactive: bool,
) -> Result<(), Error> {
  #[cfg(unix)]
  if !target.is_local() {
    ssh_broker::check_route_support(&target.to_ssh_target()?).await?;
  }
  for route in target.vpn_route()? {
    let remote_owner = route.owner.is_some();
    let client = if let Some(owner) = route.owner {
      #[cfg(unix)]
      {
        let control_path =
          ssh_broker::ensure_master_with_interaction(owner.clone(), interactive).await?;
        vpn_runtime::Client::Remote(
          ctl_ipc::remote_vpn::Client::new(owner, route.expected_remote_id)
            .with_control_path(control_path)
            .with_terminal_interaction(interactive),
        )
      }
      #[cfg(not(unix))]
      {
        let _ = owner;
        return Err(Error::RemoteVpnUnsupported);
      }
    } else {
      vpn_runtime::Client::Local(ctl_ipc::vpn::Client::default())
    };
    if !remote_owner
      && client.list().await?.connections.iter().any(|status| {
        (status.connection_id.as_deref() == Some(&route.connection_id)
          || status.vpn_id.as_deref() == Some(&route.connection_id))
          && status.state == ctl_ipc::VpnState::Connected
          && status.running
          && status.endpoint.is_some()
          && !status.status_unavailable
      })
    {
      continue;
    }
    let document = vpn_profiles::load(&vpn_profiles::path()?)?;
    let connection = document
      .connections
      .into_iter()
      .find(|connection| connection.connection_id == route.connection_id)
      .ok_or(Error::MissingVpn)?;
    if interactive {
      eprintln!("Connecting VPN {}…", connection.name);
    }
    let status = client.start_connection(connection).await?;
    if status.state != ctl_ipc::VpnState::Connected || !status.running || status.endpoint.is_none()
    {
      if let Some(url) = status.auth_url {
        if interactive {
          eprintln!("Sign in to the VPN: {url}");
        } else {
          return Err(Error::VpnAuthenticationRequired(url));
        }
      }
      return Err(Error::VpnUnavailable);
    }
  }
  Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Connect(#[from] ctl_ipc::ConnectError),
  #[error(transparent)]
  Host(#[from] HostError),
  #[error(transparent)]
  Runtime(#[from] vpn_runtime::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] ssh_broker::Error),
  #[cfg(not(unix))]
  #[error("Remote VPN management requires a Unix client.")]
  RemoteVpnUnsupported,
  #[error("Could not load the host's saved VPN. Check the VPN settings in the desktop app.")]
  MissingVpn,
  #[error(transparent)]
  Profile(#[from] vpn_profiles::Error),
  #[error("The selected VPN is not connected. Complete sign-in and retry.")]
  VpnUnavailable,
  #[error("The selected VPN requires sign-in at {0}. Complete sign-in and reconnect.")]
  VpnAuthenticationRequired(String),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn saved_selection_requires_a_literal_catalog_name_or_id() {
    let catalog = serde_json::from_value(serde_json::json!({
      "schema_version": 1, "ssh_gateways": [],
      "hosts": [{
        "host_id": "saved-1", "name": "work", "preferred_method_id": "ssh",
        "connection_methods": [{
          "method_id": "ssh", "name": "Direct",
          "target": { "kind": "ssh", "destination": "saved.example.com", "user": "saved-user" }
        }]
      }]
    }))
    .unwrap();
    for selector in ["work", "saved-1"] {
      let resolved = resolve_saved(&catalog, selector, None).unwrap();
      assert_eq!(resolved.host_id.as_deref(), Some("saved-1"));
      assert_eq!(resolved.target.label(), "saved.example.com");
    }
    for selector in ["missing", "user@work", "saved.example.com"] {
      let error = resolve_saved(&catalog, selector, None).err().unwrap();
      assert_eq!(error.code, "host_not_found");
      assert!(error.message.contains("--destination"));
    }
  }

  #[test]
  fn invalid_temporary_destinations_fail_before_transport_setup() {
    for destination in [
      "",
      "-options",
      "has space",
      "line\nfeed",
      "!excluded",
      "*",
      "host?",
    ] {
      assert_eq!(
        temporary_destination(destination).err().unwrap().code,
        "invalid_ssh_target",
      );
    }
  }

  #[tokio::test]
  async fn temporary_targets_reject_saved_options_and_default_is_local() {
    for (host, method) in [(Some("work"), None), (None, Some("vpn"))] {
      assert_eq!(
        resolve(host, Some("alias"), method)
          .await
          .err()
          .unwrap()
          .code,
        "target_conflict"
      );
    }
    assert!(resolve(None, None, None).await.unwrap().target.is_local());
    assert_eq!(
      resolve(None, None, Some("ssh")).await.err().unwrap().code,
      "host_required"
    );
  }
}
