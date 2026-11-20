//! Official input drivers and a subprocess adapter for input endpoints.
//!
//! Each driver opens a target-specific session implementing the shared input
//! capability from `ctream-driver`.

#[cfg(target_os = "linux")]
pub mod linux;
pub mod process;

#[cfg(any(windows, test))]
pub mod windows;
