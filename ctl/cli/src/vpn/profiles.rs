//! Private saved profiles shared by explicit VPN connects and host VPN routes.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use ctl_client::hosts::SavedVpnDocument;
use ctl_ipc::VpnConnection;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) fn path() -> Result<PathBuf, Error> {
  std::env::var_os("CTL_VPNS_PATH").map_or_else(
    || {
      ctl_core::paths::directory()
        .map(|directory| directory.join("vpns.json"))
        .map_err(|_| Error::HomeUnavailable)
    },
    |path| Ok(PathBuf::from(path)),
  )
}

pub(crate) fn load(path: &Path) -> Result<SavedVpnDocument, Error> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if !metadata.is_file() => return Err(Error::UnsafeFile),
    Ok(_) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      return Ok(SavedVpnDocument::default());
    }
    Err(_) => return Err(Error::Unreadable),
  }
  let mut options = OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.custom_flags(i32::from_ne_bytes(
      (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
        .bits()
        .to_ne_bytes(),
    ));
  }
  let file = options.open(path).map_err(|_| Error::Unreadable)?;
  let metadata = file.metadata().map_err(|_| Error::Unreadable)?;
  if !metadata.is_file() {
    return Err(Error::UnsafeFile);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.mode() & 0o077 != 0 || metadata.uid() != rustix::process::getuid().as_raw() {
      return Err(Error::UnsafeFile);
    }
  }
  let mut bytes = zeroize::Zeroizing::new(Vec::new());
  file
    .take(MAX_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| Error::Unreadable)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(Error::TooLarge);
  }
  // JSON parser diagnostics can contain passwords; expose only a fixed error.
  let document: SavedVpnDocument = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
  document.validate()?;
  Ok(document)
}

pub(super) fn resolve(
  mut document: SavedVpnDocument,
  selector: &str,
) -> Result<VpnConnection, Error> {
  let index = if let Some(index) = document
    .connections
    .iter()
    .position(|connection| connection.connection_id == selector)
  {
    index
  } else {
    let mut matches = document
      .connections
      .iter()
      .enumerate()
      .filter(|(_, connection)| connection.name == selector);
    let Some((index, _)) = matches.next() else {
      return Err(Error::NotFound(selector.into()));
    };
    if matches.next().is_some() {
      return Err(Error::Ambiguous(selector.into()));
    }
    index
  };
  Ok(document.connections.swap_remove(index))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Could not locate the home directory for saved VPN profiles.")]
  HomeUnavailable,
  #[error("Could not read saved VPN profiles; the file has been preserved.")]
  Unreadable,
  #[error("VPN settings must be a regular file owned by the current user and private (mode 0600).")]
  UnsafeFile,
  #[error("Saved VPN profiles exceed the size limit; the file has been preserved.")]
  TooLarge,
  #[error("Could not read vpns.json; the file has been preserved.")]
  Invalid,
  #[error(transparent)]
  Validation(#[from] ctl_client::hosts::HostError),
  #[error("No saved VPN matches {0:?}. Use `ctl vpn list` to see saved profiles.")]
  NotFound(String),
  #[error("More than one saved VPN matches {0:?}; use its VPN ID from `ctl vpn list`.")]
  Ambiguous(String),
}
