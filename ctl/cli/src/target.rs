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
  let client = if std::env::var_os("CTMUX_DEV_DAEMON_SUPERVISOR").is_some() {
    client.with_daemon_executable(ctl_ipc::default_daemon_executable()?)
  } else {
    client
  };
  if client.list().await?.connections.iter().any(|status| {
    status.vpn_id.as_deref() == Some(id) && status.state == ctl_ipc::VpnState::Connected
  }) {
    return Ok(());
  }
  let path = std::env::var_os("CTL_VPNS_PATH")
    .map(PathBuf::from)
    .or_else(|| {
      ctl_core::paths::directory()
        .ok()
        .map(|directory| directory.join("vpns.json"))
    })
    .ok_or(Error::MissingVpn)?;
  let connection = load_vpn(&path, id)?;
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

fn load_vpn(path: &std::path::Path, id: &str) -> Result<ctl_ipc::VpnConnection, Error> {
  use std::io::Read as _;
  let mut options = std::fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(i32::from_ne_bytes(
      rustix::fs::OFlags::NOFOLLOW.bits().to_ne_bytes(),
    ));
  }
  let file = options.open(path).map_err(|_| Error::MissingVpn)?;
  let metadata = file.metadata().map_err(|_| Error::MissingVpn)?;
  if !metadata.is_file() {
    return Err(Error::MissingVpn);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.mode() & 0o077 != 0 || metadata.uid() != rustix::process::getuid().as_raw() {
      return Err(Error::VpnPermissions);
    }
  }
  let mut bytes = zeroize::Zeroizing::new(Vec::new());
  file
    .take(2 * 1024 * 1024 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| Error::MissingVpn)?;
  if bytes.len() > 2 * 1024 * 1024 {
    return Err(Error::MissingVpn);
  }
  let document: hosts::SavedVpnDocument =
    serde_json::from_slice(&bytes).map_err(|_| Error::MissingVpn)?;
  document.validate()?;
  document
    .connections
    .into_iter()
    .find(|connection| connection.connection_id == id)
    .ok_or(Error::MissingVpn)
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
  #[error("VPN settings must be owned by the current user and private (mode 0600).")]
  VpnPermissions,
  #[error("The selected VPN is not connected. Complete sign-in and retry.")]
  VpnUnavailable,
}
