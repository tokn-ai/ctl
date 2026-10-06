//! Real terminal-process fixtures shared by the TUI integration cases.
mod daemon;
mod process;
mod proxy;
mod screen;

pub use daemon::TestDaemon;
pub use process::Tui;
pub use proxy::TestProxy;
pub use screen::Screen;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
