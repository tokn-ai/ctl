mod bundled;
mod updater;

#[cfg(unix)]
use ctl_client::cli::ssh_broker;
use ctl_client::cli::{RemotePlatform, component_info, table, target};

#[doc(hidden)]
pub use component_info::CONTRACTS as COMPONENT_CONTRACTS;

/// Runs the installation manager entry point.
pub fn run() {
  if let Some(code) = ctl_client::cli::openssh::run_transport_helper(bundled::register) {
    std::process::exit(code);
  }
  updater::main();
}
