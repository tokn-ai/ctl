//! Shared native Windows target identity and physical window geometry.

#[cfg(any(windows, test))]
mod id;
#[cfg(windows)]
mod monitors;
#[cfg(windows)]
mod native;
#[cfg(windows)]
mod watch;

#[cfg(windows)]
pub use id::display_number;
#[cfg(windows)]
pub use monitors::{DisplayInfo, display, list_displays};
#[cfg(windows)]
pub use native::{PhysicalPixels, WindowTarget, list_desktop_windows, list_windows};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
