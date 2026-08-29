#[cfg(unix)]
mod server;
#[cfg(unix)]
mod session;

#[cfg(unix)]
pub use server::{
  DEFAULT_ATTACHMENT_LIVENESS_TIMEOUT, DaemonConfig, DaemonError, MAX_ATTACHMENT_LIVENESS_TIMEOUT,
  MIN_ATTACHMENT_LIVENESS_TIMEOUT, run,
};

#[cfg(not(unix))]
compile_error!("rmuxd local IPC is currently implemented only for Unix platforms");
