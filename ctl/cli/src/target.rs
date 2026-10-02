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
    });
  };
  let path = catalog_path()?;
  let mut resolved = hosts::resolve(&hosts::load_catalog(&path)?, host, method)?;
  if resolved.tailscale_node_id.is_some() {
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
  let ConnectionTargetDto::Ssh {
    vpn_connection_id: Some(id),
    ..
  } = target
  else {
    return Ok(());
  };
  let client = ctl_ipc::vpn::Client::new(ctl_ipc::vpn::socket_path());
  if client.list().await?.connections.iter().any(|status| {
    status.vpn_id.as_deref() == Some(id) && status.state == ctl_ipc::VpnState::Connected
  }) {
    return Ok(());
  }
  let document = crate::vpn::profiles::load(&crate::vpn::profiles::path()?)?;
  let connection = document
    .connections
    .into_iter()
    .find(|connection| connection.connection_id == *id)
    .ok_or(Error::MissingVpn)?;
  eprintln!("Connecting VPN {}…", connection.name);
  let status = client.start_connection(connection).await?;
  if status.state != ctl_ipc::VpnState::Connected {
    if let Some(url) = status.auth_url {
      eprintln!("Sign in to the VPN: {url}");
    }
    return Err(Error::VpnUnavailable);
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
  #[error("Could not load the host's saved VPN. Check the VPN settings in the desktop app.")]
  MissingVpn,
  #[error(transparent)]
  Profile(#[from] crate::vpn::profiles::Error),
  #[error("The selected VPN is not connected. Complete sign-in and retry.")]
  VpnUnavailable,
}
