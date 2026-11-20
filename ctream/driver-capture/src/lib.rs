//! Official capture drivers, grouped by host platform and display protocol.

#[cfg(target_os = "linux")]
pub mod linux;
pub mod process;
#[cfg(windows)]
pub mod windows;

// Exercise platform-independent pixel conversion on every host.
#[cfg(all(test, not(windows)))]
#[path = "windows/pixels.rs"]
mod windows_pixels;
