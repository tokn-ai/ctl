#[cfg(unix)]
mod server;
#[cfg(unix)]
mod session;

#[cfg(unix)]
pub use server::{DaemonConfig, DaemonError, run};

#[cfg(not(unix))]
compile_error!("rmuxd local IPC is currently implemented only for Unix platforms");
