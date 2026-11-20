//! Windows display and window control through physical, per-monitor DPI-aware input.
//!
//! Keyboard events represent physical keys; Windows applies the remote keyboard
//! layout. SendInput respects Windows integrity levels and cannot control an
//! elevated application or the secure desktop from an ordinary agent.

mod protocol;
mod state;
mod window;

#[cfg(windows)]
mod native;
#[cfg(windows)]
pub use native::{Driver, Input, Session, WindowInput};
