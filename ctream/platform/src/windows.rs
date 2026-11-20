use ctream_driver::Result;
use ctream_proto::{Target, Window};

#[cfg(target_os = "linux")]
pub fn list(target: Target) -> Result<Vec<Window>> {
  use ctream_driver::{DiscoverWindows, Driver};
  use ctream_driver_capture::linux::{wayland, x11};

  match target {
    Target::X11(display) => x11::Driver.open(&display)?.windows(),
    Target::Wayland(display) => wayland::Driver.open(&display)?.windows(),
    Target::Windows(_) => Err("Windows window capture is not available yet".into()),
  }
}

#[cfg(not(target_os = "linux"))]
pub fn list(target: Target) -> Result<Vec<Window>> {
  Err(
    std::io::Error::new(
      std::io::ErrorKind::Unsupported,
      if matches!(target, Target::Windows(_)) {
        "Windows window capture is not available yet"
      } else {
        "local window discovery requires Linux"
      },
    )
    .into(),
  )
}
