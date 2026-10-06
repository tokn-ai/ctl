//! Real terminal-process fixtures shared by the TUI integration cases.
mod daemon;
mod process;
mod screen;

pub use daemon::TestDaemon;
pub use process::Tui;
pub use screen::Screen;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
