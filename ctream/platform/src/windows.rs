use ctream_driver::Result;
use ctream_proto::{Target, Window};

#[cfg(target_os = "linux")]
pub fn list(target: Target) -> Result<Vec<Window>> {
  use ctream_driver::{DiscoverWindows, Driver};
  use ctream_driver_capture::linux::{wayland, x11};

  match target {
    Target::X11(display) => x11::Driver.open(&display)?.windows(),
    Target::Wayland(display) => wayland::Driver.open(&display)?.windows(),
    Target::Windows(_) => Err("Windows windows require a native Windows agent".into()),
  }
}

#[cfg(windows)]
pub fn list(target: Target) -> Result<Vec<Window>> {
  use ctream_driver::{DiscoverWindows, Driver};

  match target {
    Target::Windows(display) => ctream_driver_capture::windows::Driver
      .open(&display)?
      .windows(),
    _ => Err("Linux windows require a Linux agent".into()),
  }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn list(target: Target) -> Result<Vec<Window>> {
  Err(
    std::io::Error::new(
      std::io::ErrorKind::Unsupported,
      format!("local window discovery for {target} requires its native platform agent"),
    )
    .into(),
  )
}
