//! A development GUI can outlive its Cargo runner and must pin its helpers.

use ctl_core::installations::{Installations, Lease};
use std::io;
use std::path::Path;

pub(super) fn retain() -> Option<Lease> {
  requested().unwrap_or_else(|error| {
    eprintln!("ctmux: could not retain the development installation: {error}");
    std::process::exit(1);
  })
}

fn requested() -> io::Result<Option<Lease>> {
  let Some(id) = std::env::var_os("CTL_DEV_INSTALLATION_ID") else {
    return Ok(None);
  };
  let id = id
    .to_str()
    .ok_or_else(|| io::Error::other("invalid development installation identity"))?;
  let home = dirs::home_dir().ok_or_else(|| io::Error::other("home directory is unavailable"))?;
  retain_at(&home, Some(id))
}

fn retain_at(home: &Path, id: Option<&str>) -> io::Result<Option<Lease>> {
  let Some(id) = id else {
    return Ok(None);
  };
  let installations = Installations::new(home);
  installations.lease(&installations.get(id)?).map(Some)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ordinary_launches_do_not_create_installation_state() {
    let home = std::env::temp_dir().join(format!("ctlup-gui-lease-{}", uuid::Uuid::new_v4()));
    assert!(retain_at(&home, None).unwrap().is_none());
    assert!(!home.exists());
    assert!(retain_at(&home, Some("invalid")).is_err());
    assert!(!home.exists());
  }
}
