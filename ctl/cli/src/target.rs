use ctl_client::hosts::{self, ConnectionTargetDto, HostError, ResolvedHost};
use std::path::PathBuf;

pub async fn resolve(host: Option<&str>, method: Option<&str>) -> Result<ResolvedHost, HostError> {
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
  let mut resolved = hosts::resolve(&hosts::load_catalog(&path)?, host, method)?;
  if resolved.requires_tailscale() {
    resolved.resolve_tailscale(&ctl_client::tailscale::discover_devices().await.devices)?;
  }
  Ok(resolved)
}

pub fn catalog_path() -> Result<PathBuf, HostError> {
  std::env::var_os("CTL_HOSTS_PATH")
    .map_or_else(
      || {
        ctl_core::paths::directory()
          .ok()
          .map(|directory| directory.join("hosts.json"))
      },
      |path| Some(PathBuf::from(path)),
    )
    .ok_or_else(|| HostError::new("home_unavailable", "Could not find the home directory."))
}

pub async fn ensure_vpn(target: &ConnectionTargetDto) -> Result<(), Error> {
  #[cfg(unix)]
  if !target.is_local() {
    crate::ssh_broker::check_route_support(&target.to_ssh_target()?).await?;
  }
  for route in target.vpn_route()? {
    let remote_owner = route.owner.is_some();
    let client = if let Some(owner) = route.owner {
      #[cfg(unix)]
      {
        let control_path = crate::ssh_broker::ensure_master(owner.clone()).await?;
        crate::vpn::RuntimeClient::Remote(
          ctl_ipc::remote_vpn::Client::new(owner, route.expected_remote_id)
            .with_control_path(control_path),
        )
      }
      #[cfg(not(unix))]
      {
        let _ = owner;
        return Err(Error::RemoteVpnUnsupported);
      }
    } else {
      crate::vpn::RuntimeClient::Local(ctl_ipc::vpn::Client::default())
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
    let document = crate::vpn::profiles::load(&crate::vpn::profiles::path()?)?;
    let connection = document
      .connections
      .into_iter()
      .find(|connection| connection.connection_id == route.connection_id)
      .ok_or(Error::MissingVpn)?;
    eprintln!("Connecting VPN {}…", connection.name);
    let status = client.start_connection(connection).await?;
    if status.state != ctl_ipc::VpnState::Connected || !status.running || status.endpoint.is_none()
    {
      if let Some(url) = status.auth_url {
        eprintln!("Sign in to the VPN: {url}");
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
  Vpn(#[from] ctl_ipc::vpn::VpnError),
  #[error(transparent)]
  Runtime(#[from] crate::vpn::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
  #[cfg(not(unix))]
  #[error("Remote VPN management requires a Unix client.")]
  RemoteVpnUnsupported,
  #[error("Could not load the host's saved VPN. Check the VPN settings in the desktop app.")]
  MissingVpn,
  #[error(transparent)]
  Profile(#[from] crate::vpn::profiles::Error),
  #[error("The selected VPN is not connected. Complete sign-in and retry.")]
  VpnUnavailable,
}
