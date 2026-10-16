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

/// One verified or declared package considered by the shared helper resolver.
/// Inspection reports its eventual immutable launch path without installing it.
#[derive(Debug, Clone, Serialize)]
pub struct HelperSelection {
  pub executable: PathBuf,
  pub info: Option<ctl_core::component::ComponentInfo>,
  pub archive_sha256: Option<String>,
  pub installed: bool,
}

/// Build-time provenance shared by debug CLI and desktop helper consumers.
pub struct DevelopmentContext {
  pub repository_root: &'static str,
  pub checkpoints: &'static [&'static str],
}

impl DevelopmentContext {
  /// Inspects this checkout's selected helper without publishing or selecting it.
  ///
  /// # Errors
  /// Rejects unsafe selections, invalid receipts, and failed native trust checks.
  pub fn inspect(
    &self,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> impl std::future::Future<Output = Result<Option<HelperSelection>, Error>> + '_ {
    #[cfg(target_os = "macos")]
    {
      async move {
        let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
        self.inspect_in(&home, required).await
      }
    }
    #[cfg(not(target_os = "macos"))]
    {
      let _ = (self, required);
      std::future::ready(Ok(None))
    }
  }

  #[cfg(target_os = "macos")]
  pub(crate) async fn inspect_in(
    &self,
    home: &std::path::Path,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> Result<Option<HelperSelection>, Error> {
    for checkpoint in self.checkpoints {
      if let Some(selection) = development::inspect(
        home,
        std::path::Path::new(checkpoint),
        std::path::Path::new(self.repository_root),
        required,
      )
      .await?
      {
        return Ok(Some(selection));
      }
    }
    Ok(None)
  }

  /// Pins this checkout's verified helper in durable storage without changing shared selections.
  ///
  /// # Errors
  /// Rejects unsafe checkpoints, invalid receipts, and failed signature checks.
  pub fn discover(
    &self,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> impl std::future::Future<Output = Result<Option<PathBuf>, Error>> + '_ {
    #[cfg(target_os = "macos")]
    {
      async move {
        let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
        self.discover_in(&home, required).await
      }
    }
    #[cfg(not(target_os = "macos"))]
    {
      let _ = (self, required);
      std::future::ready(Ok(None))
    }
  }

  #[cfg(target_os = "macos")]
  pub(crate) async fn discover_in(
    &self,
    home: &std::path::Path,
    required: Option<ctl_core::protocol::ProtocolVersion>,
  ) -> Result<Option<PathBuf>, Error> {
    for checkpoint in self.checkpoints {
      if let Some(executable) = development::discover_in(
        home,
        std::path::Path::new(checkpoint),
        std::path::Path::new(self.repository_root),
        required,
      )
      .await?
      {
        return Ok(Some(executable));
      }
    }
    Ok(None)
  }
}

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

/// Inspects the selected shared helper without installing, selecting, or starting it.
///
/// # Errors
/// Rejects invalid installation metadata, unsafe selections, and failed trust checks.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "inspection has the same API on every platform"
  )
)]
pub async fn inspect_compatible_ctld(
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Option<HelperSelection>, Error> {
  #[cfg(target_os = "macos")]
  {
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    discovery::inspect(&home, required).await
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = required;
    Ok(None)
  }
}

/// Inspects only an explicitly selected complete local bundle.
/// Ordinary managed installations are neither inspected nor modified.
///
/// # Errors
/// Rejects unsafe, invalid, incompatible, or untrusted explicit bundle selections.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "inspection has the same API on every platform"
  )
)]
pub async fn inspect_selected_bundle_ctld(
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Option<HelperSelection>, Error> {
  #[cfg(target_os = "macos")]
  {
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    discovery::inspect_selected_bundle(&home, required).await
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = required;
    Ok(None)
  }
}

/// Reads an embedded package's identity without extracting or downloading it.
/// An existing exact cache entry receives native signature and metadata checks;
/// an uncached package reports no executable metadata until preparation.
///
/// # Errors
/// Rejects invalid package receipts and unsafe, changed, or untrusted cache entries.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "inspection has the same API on every platform"
  )
)]
pub async fn inspect_bundled_ctld(
  receipt: &[u8],
  development: bool,
) -> Result<HelperSelection, Error> {
  #[cfg(target_os = "macos")]
  {
    let target = macos::release_target()?;
    let manifest = if development {
      manifest::Manifest::parse_development(receipt, env!("CARGO_PKG_VERSION"), target)?
    } else {
      manifest::Manifest::parse(receipt, env!("CARGO_PKG_VERSION"), target)?
    };
    let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
    inspect_manifest(&home, &manifest).await
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = (receipt, development);
    Err(Error::UnsupportedPlatform)
  }
}

#[cfg(target_os = "macos")]
async fn inspect_manifest(
  home: &std::path::Path,
  manifest: &manifest::Manifest,
) -> Result<HelperSelection, Error> {
  let session = install::Session::inspect_cached(home, manifest)?;
  let installed = session.is_some();
  let info = if let Some(session) = session {
    Some(macos::verify_helper(&session).await?.info)
  } else {
    None
  };
  Ok(HelperSelection {
    executable: install::installation_directory(home, manifest)
      .join("ctld.app/Contents/MacOS/ctld"),
    info,
    archive_sha256: Some(manifest.sha256.clone()),
    installed,
  })
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
/// A selected app is copied into the immutable managed development cache and
/// verified there before its executable is returned. Cargo cleanup therefore
/// cannot remove the running broker's child-helper executable.
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

/// Publishes a verified signed helper embedded in this CLI without changing
/// shared selections or starting any daemon. The immutable package receives
/// the same signature, notarization, build, and protocol checks as installation.
///
/// # Errors
/// Rejects unsupported platforms, invalid receipts, unsafe cache paths, and
/// failed archive, signature, notarization, build, or protocol verification.
#[cfg_attr(
  not(target_os = "macos"),
  expect(
    clippy::unused_async,
    reason = "preparation has the same API on every platform"
  )
)]
pub async fn prepare_bundled_ctld(
  manifest: &'static [u8],
  archive: &'static [u8],
  on_progress: impl Fn(SetupEvent) + Send + Sync,
) -> Result<SetupOutcome, Error> {
  #[cfg(target_os = "macos")]
  return macos::prepare_bundled(manifest, archive, on_progress).await;
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
