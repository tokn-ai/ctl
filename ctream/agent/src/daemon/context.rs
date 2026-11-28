//! Desktop connection locators outlive the shell that requests a live update.
//! Values identify existing sockets or authority files. No file is copied.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, io, process::Command};

#[cfg(target_os = "linux")]
const LOCATORS: &[&str] = &[
  "DISPLAY",
  "WAYLAND_DISPLAY",
  "XAUTHORITY",
  "XDG_RUNTIME_DIR",
  "DBUS_SESSION_BUS_ADDRESS",
  "AT_SPI_BUS_ADDRESS",
];
// Native desktop selection on Windows and macOS is independent of these
// environment variables (logon session and GUI capture host respectively).
#[cfg(not(target_os = "linux"))]
const LOCATORS: &[&str] = &[];

pub(super) const LIMIT: usize = 128 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct LaunchContext(BTreeMap<String, OsString>);

impl LaunchContext {
  pub(super) fn current() -> Self {
    let values: BTreeMap<_, _> = LOCATORS
      .iter()
      .filter_map(|name| std::env::var_os(name).map(|v| ((*name).to_owned(), v)))
      .collect();
    #[cfg(target_os = "linux")]
    let values = {
      let mut values = values;
      // Xlib resolves an unset XAUTHORITY relative to HOME. SSH launchers
      // may also run in a different working directory than the desktop.
      let authority = match values.get("XAUTHORITY") {
        Some(path) => (!path.is_empty()).then(|| std::path::PathBuf::from(path)),
        None => {
          std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".Xauthority"))
        }
      };
      if let Some(authority) = authority.and_then(|p| std::path::absolute(p).ok()) {
        values.insert("XAUTHORITY".into(), authority.into_os_string());
      }
      values
    };
    Self(values)
  }

  pub(super) fn apply(&self, command: &mut Command) -> io::Result<()> {
    if self.0.keys().any(|name| !LOCATORS.contains(&name.as_str())) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "unknown desktop launch locator",
      ));
    }
    for &name in LOCATORS {
      match self.0.get(name) {
        Some(value) => command.env(name, value),
        None => command.env_remove(name),
      };
    }
    #[cfg(target_os = "linux")]
    command.env_remove("WAYLAND_SOCKET"); // descriptor from a different process
    Ok(())
  }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use super::*;
  use std::{ffi::OsStr, os::unix::ffi::OsStringExt as _};

  #[test]
  fn handoff_preserves_native_path_bytes_and_clears_foreign_desktop_locators() {
    let path = OsString::from_vec(vec![b'/', b'x', 0xfe]);
    let saved = LaunchContext(BTreeMap::from([("XAUTHORITY".into(), path.clone())]));
    let saved: LaunchContext =
      serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
    let mut command = Command::new("unused");
    command
      .env("DISPLAY", ":1")
      .env("WAYLAND_SOCKET", "8")
      .env("UNRELATED", "local");
    saved.apply(&mut command).unwrap();
    let env: BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(env[OsStr::new("XAUTHORITY")], Some(path.as_os_str()));
    assert_eq!(env[OsStr::new("DISPLAY")], None);
    assert_eq!(env[OsStr::new("WAYLAND_SOCKET")], None);
    assert_eq!(env[OsStr::new("UNRELATED")], Some(OsStr::new("local")));
    assert!(
      LaunchContext(BTreeMap::from([("UNRELATED".into(), "foreign".into())]))
        .apply(&mut command)
        .is_err()
    );
  }
}
