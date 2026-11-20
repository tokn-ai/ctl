//! Display discovery and accessibility adapters used by the shipped agent.

#[cfg(target_os = "linux")]
pub mod controls;
#[cfg(target_os = "linux")]
mod discovery;
pub mod windows;

#[cfg(target_os = "linux")]
pub use discovery::discover;

#[cfg(not(any(target_os = "linux", windows)))]
pub fn discover() -> std::io::Result<Vec<(ctream_proto::Display, Option<String>)>> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "local display discovery requires Linux or Windows",
  ))
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn discover_sessions() -> ctream_driver::Result<Vec<ctream_proto::Session>> {
  Err("local display discovery requires Linux or Windows".into())
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
          Display::X11(_) | Display::Windows { .. } => true,
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

#[cfg(windows)]
pub fn discover_sessions() -> ctream_driver::Result<Vec<ctream_proto::Session>> {
  use ctream_proto::{Display, Session};
  Ok(
    ctream_driver_capture::windows::discover()?
      .into_iter()
      .map(|display| Session {
        display: Display::Windows {
          number: display.number,
          name: display.name,
          bounds: display.bounds,
        },
        class: Some("interactive".into()),
        selectable: true,
      })
      .collect(),
  )
}

#[cfg(windows)]
pub fn discover() -> std::io::Result<Vec<(ctream_proto::Display, Option<String>)>> {
  discover_sessions()
    .map(|sessions| {
      sessions
        .into_iter()
        .map(|session| (session.display, session.class))
        .collect()
    })
    .map_err(|error| std::io::Error::other(error.to_string()))
}
