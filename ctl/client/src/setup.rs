//! Explicit installation of the signed, per-user macOS connection helper.

#[cfg(any(target_os = "macos", all(test, unix)))]
mod archive;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod install;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod manifest;
#[cfg(all(test, unix))]
mod tests;

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub enum SetupEvent {
  Manifest,
  Downloading {
    received_bytes: u64,
    total_bytes: u64,
  },
  Extracting,
  Verifying,
  Activating,
}

#[derive(Debug, Serialize)]
pub struct SetupOutcome {
  pub component: &'static str,
  pub version: String,
  pub executable: PathBuf,
  pub reused: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("ctl setup installs the signed macOS ctld helper; this platform is not supported")]
  UnsupportedPlatform,
  #[error("could not determine the current user's home directory")]
  HomeDirectory,
  #[error("invalid ctld release: {0}")]
  InvalidRelease(String),
  #[error("ctld setup failed: {0}")]
  Io(#[from] std::io::Error),
  #[error("another ctld setup is running; try again after it finishes")]
  Busy,
  #[error("could not download the signed ctld release: {0}")]
  Download(String),
  #[error("signed ctld verification failed: {0}")]
  Verification(String),
}

/// Installs the signed helper for this CLI's release without starting or stopping
/// any daemon. Downloaded code is verified before its metadata query is executed.
///
/// # Errors
/// Rejects unsupported platforms, unavailable releases, unsafe installation
/// paths, concurrent setup, invalid archives, or failed signature/protocol checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "unsupported platforms retain the same asynchronous setup API"
  )
)]
pub async fn install_signed_ctld(
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  #[cfg(target_os = "macos")]
  return macos::install(on_progress).await;
  #[cfg(not(target_os = "macos"))]
  {
    let _ = on_progress;
    Err(Error::UnsupportedPlatform)
  }
}
