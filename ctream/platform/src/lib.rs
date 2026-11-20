//! Display discovery and accessibility adapters used by the shipped agent.

#[cfg(target_os = "linux")]
pub mod controls;
#[cfg(target_os = "linux")]
mod discovery;
pub mod windows;

#[cfg(target_os = "linux")]
pub use discovery::discover;

#[cfg(not(target_os = "linux"))]
pub fn discover() -> std::io::Result<Vec<(ctream_proto::Display, Option<String>)>> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "local display discovery requires Linux",
  ))
}

#[cfg(target_os = "linux")]
pub fn discover_sessions() -> ctream_driver::Result<Vec<ctream_proto::Session>> {
  use ctream_proto::{Display, Session};
  use std::{env, fs, os::unix::fs::MetadataExt};

  let runtime_uid = env::var_os("XDG_RUNTIME_DIR")
    .and_then(|path| fs::metadata(path).ok())
    .map(|metadata| metadata.uid());
  Ok(
    discover()?
      .into_iter()
      .map(|(display, class)| {
        let selectable = match &display {
          Display::X11(_) => true,
          Display::Wayland { uid, .. } => runtime_uid == Some(*uid),
        };
        Session {
          display,
          class,
          selectable,
        }
      })
      .collect(),
  )
}
