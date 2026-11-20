//! Native display discovery and capability providers for the display host.

mod discovery;
#[cfg(unix)]
pub mod providers;
pub mod windows;

pub use discovery::discover;
