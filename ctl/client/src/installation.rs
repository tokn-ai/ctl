//! Installation operations shared by ctlup, the desktop, and remote bootstrap.

use ctl_core::bundles::{Bundle, MANIFEST_FILE, Manifest};
use ctl_core::component_update::BundleChannel;
use ctl_core::installations::Installations;
use ctl_core::private_fs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

mod provenance;
mod remote;
mod show;
#[cfg(test)]
mod tests;
mod updater;
pub(crate) use provenance::single_target_heads;
pub use provenance::{activate_uploaded, decode_catalog_origin, encode_catalog_origin};
pub use remote::{
  InstallationCatalog, remote_account_id, remote_catalog, remote_runtime_source, remote_show,
  remote_status, verify_remote_activation,
};
pub use show::{InstallationShow, InstalledBuild, show};
pub use updater::{
  provision_manager, provision_manager_from, updater_for_bundle, updater_for_manifest,
};
pub(crate) use updater::{
  provision_manager_bytes, provision_manager_from_for_build, provision_manager_with_origin,
  remember_supplied_updater, remember_verified_updater, supplied_updater_info,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationStatus {
  #[serde(deserialize_with = "Deserialize::deserialize")]
  pub active: Option<Manifest>,
  #[serde(deserialize_with = "Deserialize::deserialize")]
  pub previous: Option<Manifest>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub missing_builds: Vec<String>,
}

/// The selected runtime and its source, including a selection with missing bytes.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSource {
  pub active: Option<String>,
  pub missing: bool,
  pub origin: Option<ctl_core::installations::CatalogOrigin>,
  pub target_triple: Option<String>,
}

impl RuntimeSource {
  fn validate(&self) -> io::Result<()> {
    match self.active.as_deref() {
      Some(id) if valid_build_id(id) => (),
      None if !self.missing && self.origin.is_none() && self.target_triple.is_none() => (),
      _ => return Err(invalid("invalid runtime source selection")),
    }
    if let Some(origin) = &self.origin {
      origin.validate()?;
    }
    if let Some(target) = &self.target_triple {
      ctl_core::bundles::validate_target(target)?;
    }
    Ok(())
  }
}

/// Returns the runtime source recorded for this installation profile.
///
/// # Errors
/// Reports invalid registry and payload metadata.
pub fn runtime_source(home: &Path) -> io::Result<RuntimeSource> {
  let inspection = Installations::new(home).inspect_status()?;
  let missing = inspection
    .active
    .as_ref()
    .is_some_and(|id| inspection.missing.contains(id));
  let result = RuntimeSource {
    target_triple: inspection
      .active
      .as_ref()
      .and_then(|id| {
        inspection
          .entries
          .iter()
          .find(|entry| entry.bundle_id == *id)
      })
      .map(|entry| entry.target_triple.clone()),
    active: inspection.active,
    origin: inspection.active_origin,
    missing,
  };
  result.validate()?;
  Ok(result)
}

impl InstallationStatus {
  fn validate(&self) -> io::Result<()> {
    let missing = validate_missing_builds(&self.missing_builds)?;
    for manifest in [self.active.as_ref(), self.previous.as_ref()]
      .into_iter()
      .flatten()
    {
      manifest.validate()?;
      if missing.contains(manifest.bundle_id.as_str()) {
        return Err(invalid("selected build is both present and missing"));
      }
    }
    Ok(())
  }
}

const MAX_INSPECTED_BUILDS: usize = 4096;

fn valid_build_id(id: &str) -> bool {
  id.len() == 64
    && id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_missing_builds(builds: &[String]) -> io::Result<BTreeSet<&str>> {
  if builds.len() > MAX_INSPECTED_BUILDS {
    return Err(invalid("too many missing installed builds"));
  }
  let mut missing = BTreeSet::new();
  for id in builds {
    if !valid_build_id(id) {
      return Err(invalid("invalid missing installation identity"));
    }
    if !missing.insert(id.as_str()) {
      return Err(invalid("duplicate missing installation identity"));
    }
  }
  Ok(missing)
}

#[derive(Debug, Default, Serialize)]
pub struct CachePruneResult {
  pub download_bytes: u64,
  pub extracted_bytes: u64,
  pub updater_bytes: u64,
}

/// Keep cached deployment data bounded without touching active installations.
/// Prepared extracted builds hold OS leases, so another update can finish safely.
/// Exact cached bundles and updater records for missing builds remain available
/// for recovery, even if doing so exceeds their cache budgets.
///
/// # Errors
/// Reports unsafe cache entries or failed cleanup; callers must preserve any
/// already completed installation and report cleanup independently.
pub fn prune_caches(home: &Path) -> io::Result<CachePruneResult> {
  let store = Installations::new(home);
  let root = store.updater_root().join("cache");
  if !storage_exists(&root)? {
    return Ok(CachePruneResult::default());
  }
  store.with_locked_manifests(|manifests, missing| {
    let recovery_pins = missing
      .iter()
      .map(|entry| (entry.bundle_id.as_str(), entry.target_triple.as_str()))
      .collect::<BTreeSet<_>>();
    let download_bytes =
      crate::remote_bundle::prune_download_cache(&root.join("downloads"), 256 * 1024 * 1024)?;
    let extracted = ctl_core::bundles::Store::cache(home)
      .prune_cache_with_pins(512 * 1024 * 1024, &recovery_pins)?;
    let updater_bytes = crate::remote_bundle::published::prune_updater_cache_with_pins(
      &root.join("updaters"),
      128 * 1024 * 1024,
      manifests,
      &recovery_pins,
    )?;
    Ok(CachePruneResult {
      download_bytes,
      extracted_bytes: extracted.remaining_bytes,
      updater_bytes,
    })
  })
}

/// Inspect native selections and report missing installed payloads.
///
/// # Errors
/// Reports corrupt or inaccessible installation state.
pub fn status(home: &Path) -> io::Result<InstallationStatus> {
  let inspection = Installations::new(home).inspect_status()?;
  let mut manifests = inspection
    .bundles
    .into_iter()
    .map(|bundle| (bundle.manifest.bundle_id.clone(), bundle.manifest))
    .collect::<BTreeMap<_, _>>();
  let status = InstallationStatus {
    active: inspection.active.and_then(|id| manifests.remove(&id)),
    previous: inspection.previous.and_then(|id| manifests.remove(&id)),
    missing_builds: inspection.missing,
  };
  status.validate()?;
  Ok(status)
}

/// Resolves the account's desktop app directory.
/// # Errors
/// Reports unsafe storage and errors creating the directory when requested.
pub fn app_directory(home: &Path, create: bool) -> io::Result<PathBuf> {
  let directory = Installations::new(home).root().join("apps");
  if create {
    safe_directory(&directory)?;
  } else {
    storage_exists(&directory)?;
  }
  Ok(directory)
}

/// Decode and verify a bounded complete managed installation archive.
///
/// # Errors
/// Rejects unsafe paths, mixed builds, missing files and invalid checksums.
pub fn unpack_archive(bytes: &[u8]) -> io::Result<(Manifest, BTreeMap<String, Vec<u8>>)> {
  let mut files = crate::components::read_archive(bytes)?;
  let manifest = crate::components::inspect_upload_archive(&mut files)?
    .ok_or_else(|| invalid("ctlup requires a complete managed installation archive"))?;
  if !crate::components::compatible(&manifest.components) {
    return Err(invalid(
      "installation does not support this updater's contracts",
    ));
  }
  Ok((manifest, files))
}

/// Verify, stage and activate an uploaded archive without restarting services.
///
/// # Errors
/// Rejects corrupt archives, unsafe storage, wrong targets and failed activation.
pub async fn install_archive(home: &Path, bytes: &[u8]) -> io::Result<Bundle> {
  install_archive_with_progress(home, bytes, |_| {}).await
}

/// Activate a complete upload while reporting the existing receiver progress format.
///
/// # Errors
/// Reports verification, staging or atomic activation failures.
pub async fn install_archive_with_progress(
  home: &Path,
  bytes: &[u8],
  on_progress: impl Fn(crate::RemoteInstallEvent) + Send + Sync + 'static,
) -> io::Result<Bundle> {
  install_archive_with_origin(home, bytes, None, None, on_progress).await
}

/// Activate an uploaded complete build with its separately validated provenance.
/// Existing progress framing and archive bytes remain unchanged.
///
/// # Errors
/// Reports malformed provenance, verification, staging or activation failures.
pub async fn install_archive_with_origin(
  home: &Path,
  bytes: &[u8],
  origin: Option<ctl_core::installations::CatalogOrigin>,
  manager: Option<&Path>,
  on_progress: impl Fn(crate::RemoteInstallEvent) + Send + Sync + 'static,
) -> io::Result<Bundle> {
  if let Some(origin) = &origin {
    origin.validate()?;
  }
  let home = home.to_owned();
  let staging_home = home.clone();
  let bytes = bytes.to_vec();
  let (stage, on_progress, build) = tokio::task::spawn_blocking(move || {
    on_progress(crate::RemoteInstallEvent::Extracting);
    let (manifest, files) = unpack_archive(&bytes)?;
    if !ctl_core::bundles::local_target(&manifest.target_triple) {
      return Err(invalid("archive installation requires a native build"));
    }
    let layout = ctl_core::bundle_layout::Layout::managed(manifest.schema_version)
      .ok_or_else(|| invalid("unknown installation layout"))?;
    for &file_name in layout.names() {
      on_progress(crate::RemoteInstallEvent::Checking { file_name });
    }
    let build = manifest.components["ctl-agent"].build.clone();
    Ok((
      stage_bundle(&staging_home, &manifest, &files)?,
      on_progress,
      build,
    ))
  })
  .await
  .map_err(io::Error::other)??;
  if let Some(manager) = manager {
    provision_manager_from_for_build(&home, manager, origin.as_ref(), &build).await?;
  }
  tokio::task::spawn_blocking(move || {
    on_progress(crate::RemoteInstallEvent::Activating);
    let installations = Installations::new(&home);
    let bundle = Bundle::open(&stage.0)?;
    let (installed, _lease) = activate_uploaded(&home, &bundle, origin.as_ref())?;
    // Cleanup cannot turn an already successful activation into an apparent
    // failed update. A later explicit cleanup can retry it safely.
    if let Err(error) = installations.prune_unused() {
      eprintln!("ctlup: installed successfully; cleanup deferred: {error}");
    }
    on_progress(crate::RemoteInstallEvent::Complete);
    Ok(installed)
  })
  .await
  .map_err(io::Error::other)?
}

fn stage_bundle(
  home: &Path,
  manifest: &Manifest,
  files: &BTreeMap<String, Vec<u8>>,
) -> io::Result<TemporaryDirectory> {
  let directory = ctl_core::paths::updater_directory_for_account(home).join("staging");
  safe_directory(&directory)?;
  let path = directory.join(format!("archive-{}", uuid::Uuid::new_v4()));
  private_fs::create_directory(&path)?;
  let stage = TemporaryDirectory(path);
  for (name, bytes) in files {
    let destination = stage.0.join(name);
    if let Some(parent) = destination.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let mut file = private_fs::options()
      .write(true)
      .create_new(true)
      .open(&destination)?;
    file.write_all(bytes)?;
    if manifest.files[name].executable {
      private_fs::set_executable(&file)?;
    }
    file.sync_all()?;
  }
  let mut file = private_fs::options()
    .write(true)
    .create_new(true)
    .open(stage.0.join(MANIFEST_FILE))?;
  serde_json::to_writer(&mut file, manifest).map_err(io::Error::other)?;
  file.sync_all()?;
  Ok(stage)
}

pub(crate) struct TemporaryDirectory(pub(crate) PathBuf);

impl TemporaryDirectory {
  pub(crate) fn new(kind: &str) -> io::Result<Self> {
    let path = std::env::temp_dir().join(format!("ctlup-{kind}-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    Ok(Self(path))
  }
}

impl Drop for TemporaryDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// Returns the current runtime's tracked publication, or the distribution default.
///
/// # Errors
/// Reports invalid account storage.
pub fn default_channel(home: &Path) -> io::Result<BundleChannel> {
  let tracked = Installations::new(home).active_origin()?;
  Ok(
    match tracked.as_ref().map(|origin| origin.channel.as_str()) {
      Some("stable") => BundleChannel::Stable,
      Some("nightly") => BundleChannel::Nightly,
      _ => BundleChannel::default(),
    },
  )
}

pub(super) fn safe_directory(path: &Path) -> io::Result<()> {
  safe_directory_at(path, ctl_core::paths::managed_storage_anchor(path)?)
}

fn safe_directory_at(path: &Path, home: &Path) -> io::Result<()> {
  let mut directory = home.to_owned();
  let relative = path.strip_prefix(home).map_err(io::Error::other)?;
  for part in std::iter::once(None).chain(relative.components().map(Some)) {
    if let Some(part) = part {
      if !matches!(part, std::path::Component::Normal(_)) {
        return Err(invalid("invalid ctlup storage path"));
      }
      directory.push(part);
    }
    match std::fs::symlink_metadata(&directory) {
      Ok(metadata) => {
        if !private_fs::is_directory(&metadata) {
          return Err(invalid(
            "ctlup storage must be owned directories, not links or writable by others",
          ));
        }
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        match private_fs::create_directory(&directory) {
          Ok(()) => {}
          Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&directory)?;
            if !private_fs::is_directory(&metadata) {
              return Err(invalid("unsafe concurrent ctlup storage creation"));
            }
          }
          Err(error) => return Err(error),
        }
      }
      Err(error) => return Err(error),
    }
  }
  Ok(())
}

pub(super) fn storage_exists(path: &Path) -> io::Result<bool> {
  storage_exists_at(path, ctl_core::paths::managed_storage_anchor(path)?)
}

fn storage_exists_at(path: &Path, home: &Path) -> io::Result<bool> {
  let mut directory = home.to_owned();
  let relative = path.strip_prefix(home).map_err(io::Error::other)?;
  for part in std::iter::once(None).chain(relative.components().map(Some)) {
    if let Some(part) = part {
      if !matches!(part, std::path::Component::Normal(_)) {
        return Err(invalid("invalid ctlup storage path"));
      }
      directory.push(part);
    }
    let metadata = match std::fs::symlink_metadata(&directory) {
      Ok(metadata) => metadata,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
      Err(error) => return Err(error),
    };
    if !private_fs::is_directory(&metadata) {
      return Err(invalid(
        "ctlup storage must be owned directories, not links or writable by others",
      ));
    }
  }
  Ok(true)
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}
