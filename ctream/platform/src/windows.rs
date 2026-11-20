use ctream_driver::Result;
use ctream_proto::{Target, Window};

#[cfg(target_os = "linux")]
pub fn list(target: Target) -> Result<Vec<Window>> {
  use ctream_driver::{DiscoverWindows, Driver};
  use ctream_driver_capture::linux::{wayland, x11};

  match target {
    Target::X11(display) => x11::Driver.open(&display)?.windows(),
    Target::Wayland(display) => wayland::Driver.open(&display)?.windows(),
  }
}

#[cfg(not(target_os = "linux"))]
pub fn list(_target: Target) -> Result<Vec<Window>> {
  Err(
    std::io::Error::new(
      std::io::ErrorKind::Unsupported,
      "local window discovery requires Linux",
    )
    .into(),
  )
}
