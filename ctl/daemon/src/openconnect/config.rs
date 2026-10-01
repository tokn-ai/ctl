//! Private configuration snapshots shared by status and the container adapter.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use ctld_ipc::{VpnConnection, VpnSettings};
use zeroize::Zeroizing;

// One base64 line must fit the entrypoint's 65,536-byte input limit.
const MAX_ENV_BYTES: u64 = 48 * 1024;

#[derive(Clone, Default)]
pub(crate) struct Metadata {
  pub(crate) vpn_url: Option<String>,
  pub(crate) username: Option<String>,
}

pub(crate) struct Config {
  pub(crate) content: Zeroizing<String>,
  pub(crate) metadata: Metadata,
  pub(crate) runtime: crate::vpn_container::RuntimeMetadata,
  pub(crate) cancellation: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl Config {
  pub(crate) fn from_connection(connection: &VpnConnection) -> io::Result<Self> {
    connection
      .validate()
      .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let VpnSettings::Openconnect {
      url,
      username,
      password,
      auth_method,
      target_ip,
    } = &connection.settings
    else {
      return Err(invalid("expected an OpenConnect profile"));
    };
    let mut content = Zeroizing::new(String::new());
    for (key, value) in [
      ("VPN_URL", url.as_str()),
      ("VPN_USERNAME", username.as_str()),
      ("VPN_PASSWORD", password.as_str()),
      (
        "VPN_AUTH_METHOD",
        auth_method.as_deref().unwrap_or_default(),
      ),
      ("TARGET_IP", target_ip.as_deref().unwrap_or_default()),
    ] {
      content.push_str(key);
      content.push('=');
      content.push_str(value);
      content.push('\n');
    }
    Ok(Self {
      content,
      metadata: Metadata::new(url, username)?,
      runtime: crate::vpn_container::RuntimeMetadata::for_connection(connection)?,
      cancellation: None,
    })
  }
}

impl Metadata {
  fn new(url: &str, username: &str) -> io::Result<Self> {
    if username.is_empty() || username.len() > 256 || username.chars().any(char::is_control) {
      return Err(invalid(
        "VPN username must be nonempty, at most 256 bytes, and contain no control characters",
      ));
    }
    Ok(Self {
      vpn_url: Some(display_gateway(url)?),
      username: Some(username.to_owned()),
    })
  }
}

fn invalid(message: &'static str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn display_gateway(url: &str) -> io::Result<String> {
  if url.is_empty()
    || url.len() > 2048
    || url.chars().any(char::is_whitespace)
    || url.chars().any(char::is_control)
  {
    return Err(invalid(
      "VPN URL must be a nonempty HTTPS gateway without whitespace or control characters",
    ));
  }
  let address = if let Some(address) = url.strip_prefix("https://") {
    address
  } else {
    if url.contains("://") {
      return Err(invalid("VPN URL must use HTTPS"));
    }
    url
  };
  if address.contains('\\') {
    return Err(invalid("VPN URL must contain a valid gateway address"));
  }
  let authority = address.split(['/', '?', '#']).next().unwrap_or_default();
  let authority = authority.rsplit('@').next().unwrap_or_default();
  let parsed = url::Url::parse(&format!("https://{authority}"))
    .map_err(|_| invalid("VPN URL must contain a valid gateway address"))?;
  if parsed.host_str().is_none() {
    return Err(invalid("VPN URL must contain a valid gateway address"));
  }
  // Origin excludes userinfo, query tokens, fragments, and paths that may
  // themselves carry authentication tokens.
  Ok(parsed.origin().ascii_serialization())
}

/// Take one bounded snapshot on the blocking pool while the actor retains
/// cancellation ownership. Docker receives these same immutable bytes.
pub(crate) async fn read_file(path: PathBuf) -> io::Result<(PathBuf, Config)> {
  tokio::task::spawn_blocking(move || read_file_blocking(&path))
    .await
    .map_err(|_| io::Error::other("could not read VPN configuration"))?
}

fn read_file_blocking(path: &Path) -> io::Result<(PathBuf, Config)> {
  let path = path
    .canonicalize()
    .map_err(|_| io::Error::other("could not open VPN env file"))?;
  // NONBLOCK prevents a replacement FIFO from occupying a blocking-pool worker.
  #[cfg(unix)]
  let file = File::from(
    rustix::fs::open(
      &path,
      rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::empty(),
    )
    .map_err(|_| io::Error::other("could not open VPN env file"))?,
  );
  #[cfg(not(unix))]
  let file = File::open(&path).map_err(|_| io::Error::other("could not open VPN env file"))?;
  let metadata = file
    .metadata()
    .map_err(|_| io::Error::other("could not inspect VPN env file"))?;
  if !metadata.is_file() {
    return Err(invalid("VPN env file must be a regular file"));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
      return Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "VPN env file must be private; run chmod 600 on the file",
      ));
    }
  }
  if metadata.len() > MAX_ENV_BYTES {
    return Err(invalid("VPN env file must be at most 48 KiB"));
  }
  let mut content = Zeroizing::new(String::new());
  file
    .take(MAX_ENV_BYTES + 1)
    .read_to_string(&mut content)
    .map_err(|_| invalid("VPN env file must contain valid UTF-8 text"))?;
  if content.len() as u64 > MAX_ENV_BYTES {
    return Err(invalid("VPN env file must be at most 48 KiB"));
  }
  let metadata = parse_metadata(&content)?;
  let runtime = crate::vpn_container::RuntimeMetadata::for_env(
    &path,
    &content,
    metadata.vpn_url.clone(),
    metadata.username.clone(),
  )?;
  Ok((
    path,
    Config {
      content,
      metadata,
      runtime,
      cancellation: None,
    },
  ))
}

fn parse_metadata(content: &str) -> io::Result<Metadata> {
  if content.contains(['\r', '\0']) {
    return Err(invalid(
      "VPN env values cannot contain carriage returns or NUL",
    ));
  }
  let mut url = "";
  let mut username = "";
  let mut password_present = false;
  // Match entrypoint.sh's literal parser: no quotes, expansion, whitespace
  // trimming, or inline comments; duplicate assignments use the last value.
  for line in content.split('\n') {
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let (key, value) = line
      .split_once('=')
      .ok_or_else(|| invalid("VPN env file contains an invalid setting"))?;
    match key {
      "VPN_URL" => url = value,
      "VPN_USERNAME" => username = value,
      "VPN_PASSWORD" => password_present = !value.is_empty(),
      "VPN_AUTH_METHOD" | "TARGET_IP" => {}
      _ => return Err(invalid("VPN env file contains an unknown setting")),
    }
  }
  if !password_present {
    return Err(invalid("VPN password is required"));
  }
  Metadata::new(url, username)
}

#[cfg(test)]
mod tests;
