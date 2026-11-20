//! Official capture drivers, grouped by host platform and display protocol.

#[cfg(target_os = "linux")]
pub mod linux;
pub mod process;
