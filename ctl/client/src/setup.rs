//! Installation of the signed, per-user macOS connection helper.

#[cfg(any(target_os = "macos", all(test, unix)))]
mod archive;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod development;
#[cfg(target_os = "macos")]
mod discovery;
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

/// Verifies a complete signed helper package before its metadata is executed.
/// The temporary verification work is discarded; selection and services are unchanged.
///
/// # Errors
/// Rejects malformed receipts, invalid signatures, profiles or changed helper bytes.
#[cfg(target_os = "macos")]
pub async fn inspect_ctld_package(
  home: &std::path::Path,
  directory: &std::path::Path,
  receipt: &[u8],
) -> Result<ctl_core::component::ComponentInfo, Error> {
  let manifest = manifest::Manifest::parse_installed(receipt, macos::release_target()?)?;
  let session = install::Session::verification(home, directory, manifest)?;
  Ok(macos::verify_helper(&session).await?.info)
}

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

/// Discovers a selected shared macOS helper without downloading or restarting it.
/// Both signed releases and provisioned development apps are reusable when their
/// own installation identity is verified and the required helper APIs match.
///
/// # Errors
/// Rejects unsafe selections, invalid installation metadata, failed Apple
/// signature/provisioning checks, or unavailable trust-service inspection.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "discovery retains its asynchronous API on other platforms"
  )
)]
pub async fn discover_compatible_ctld() -> Result<Option<PathBuf>, Error> {
  #[cfg(target_os = "macos")]
  {
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    discovery::discover(&home).await
  }
  #[cfg(not(target_os = "macos"))]
  Ok(None)
}

/// Discovers a verified shared helper that explicitly implements one helper
/// contract in addition to the client's general daemon compatibility needs.
/// A compatible managed helper without that contract is left selected and
/// returns `None`, allowing the caller to prepare its own bundled helper.
///
/// # Errors
/// Rejects unsafe selections and failed trust checks. An explicitly selected
/// local bundle without the required contract is rejected instead of bypassed.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "discovery retains its asynchronous API on other platforms"
  )
)]
pub async fn discover_ctld_for_helper_contract(
  required: ctl_core::protocol::ProtocolVersion,
) -> Result<Option<PathBuf>, Error> {
  #[cfg(target_os = "macos")]
  {
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    discovery::discover_for_helper_contract(&home, required).await
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = required;
    Ok(None)
  }
}

/// Discovers a signed development helper provisioned for one checkout without
/// changing any shared selection. The checkpoint is that checkout's private
/// directory under `ctl-dev/helpers`, keyed by its canonical repository path.
/// Supply the canonical repository path recorded at build time; the source
/// checkout does not need to remain accessible when this function runs.
/// A missing selection or valid incompatible helper returns `None`.
///
/// # Errors
/// Rejects foreign checkout or target selections, unsafe paths, malformed
/// development receipts, and failed Apple signature or provisioning checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "discovery retains its asynchronous API on other platforms"
  )
)]
pub async fn discover_development_ctld(
  checkpoint: &std::path::Path,
  repository_root: &std::path::Path,
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Option<PathBuf>, Error> {
  #[cfg(target_os = "macos")]
  return development::discover(checkpoint, repository_root, required).await;
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (checkpoint, repository_root, required);
    Ok(None)
  }
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

/// Installs a signed helper embedded in this CLI without downloading or starting
/// any daemon. The complete app is verified with the same release policy as
/// downloaded helpers before its metadata query is executed.
///
/// # Errors
/// Rejects unsupported platforms, mismatched releases, unsafe installation
/// paths, concurrent setup, invalid archives, or failed signature/protocol checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "unsupported platforms retain the same asynchronous setup API"
  )
)]
pub async fn install_bundled_ctld(
  manifest: &'static [u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  #[cfg(target_os = "macos")]
  return macos::install_bundled(manifest, archive, on_progress).await;
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (manifest, archive, on_progress);
    Err(Error::UnsupportedPlatform)
  }
}

/// Installs an explicitly provisioned local development helper in its separate
/// content-addressed cache. Requires an Apple-signed app and matching profile,
/// verifies source identity and protocols, and leaves production selection alone.
///
/// # Errors
/// Rejects unsupported platforms, mismatched development metadata, untrusted
/// installation paths, invalid archives, and failed signature or profile checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "unsupported platforms retain the asynchronous API"
  )
)]
pub async fn install_bundled_development_ctld(
  manifest: &'static [u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  #[cfg(target_os = "macos")]
  return macos::install_bundled_development(manifest, archive, on_progress).await;
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (manifest, archive, on_progress);
    Err(Error::UnsupportedPlatform)
  }
}

/// Caches and pins a signed helper embedded in a development CLI without
/// changing the shared selection or starting any daemon. Verification follows
/// the same development signature, profile, source and protocol requirements
/// as explicit installation.
///
/// # Errors
/// Rejects unsupported platforms, mismatched development metadata, untrusted
/// installation paths, invalid archives, and failed signature or profile checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "unsupported platforms retain the asynchronous API"
  )
)]
pub async fn prepare_bundled_development_ctld(
  manifest: &'static [u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  #[cfg(target_os = "macos")]
  return macos::prepare_bundled_development(manifest, archive, on_progress).await;
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (manifest, archive, on_progress);
    Err(Error::UnsupportedPlatform)
  }
}
