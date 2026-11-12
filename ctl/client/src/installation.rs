//! Installation operations shared by ctlup, the desktop, and remote bootstrap.

use ctl_core::bundles::{Bundle, MANIFEST_FILE, Manifest};
use ctl_core::component_update::BundleChannel;
use ctl_core::installations::Installations;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

mod remote;
#[cfg(test)]
mod tests;
mod updater;
pub use remote::{remote_rollback, remote_status};
pub use updater::{
  provision_manager, provision_manager_from, updater_for_bundle, updater_for_manifest,
};
pub(crate) use updater::{provision_manager_bytes, remember_supplied_updater};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationStatus {
  pub active: Option<Manifest>,
  pub previous: Option<Manifest>,
}

#[derive(Debug, Serialize)]
pub struct CachePruneResult {
  pub download_bytes: u64,
  pub extracted_bytes: u64,
  pub updater_bytes: u64,
}

/// Keep cached deployment data bounded without touching active installations.
/// Prepared extracted builds hold OS leases, so another update can finish safely.
///
/// # Errors
/// Reports unsafe cache entries or failed cleanup; callers must preserve any
/// already completed installation and report cleanup independently.
pub fn prune_caches(home: &Path) -> io::Result<CachePruneResult> {
  let download_bytes = crate::remote_bundle::prune_download_cache(
    &home.join(".tokn/ctl/cache/downloads"),
    256 * 1024 * 1024,
  )?;
  let result = ctl_core::bundles::Store::cache(home).prune_cache(512 * 1024 * 1024)?;
  let updater_bytes = crate::remote_bundle::published::prune_updater_cache(
    &home.join(".tokn/ctl/cache/updaters"),
    128 * 1024 * 1024,
  )?;
  Ok(CachePruneResult {
    download_bytes,
    extracted_bytes: result.remaining_bytes,
    updater_bytes,
  })
}

/// Inspect the active and rollback builds without changing files or services.
///
/// # Errors
/// Reports corrupt or inaccessible installation state.
pub fn status(home: &Path) -> io::Result<InstallationStatus> {
  let installations = Installations::new(home);
  Ok(InstallationStatus {
    active: installations.active()?.map(|bundle| bundle.manifest),
    previous: installations.previous()?.map(|bundle| bundle.manifest),
  })
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
  let home = home.to_owned();
  let bytes = bytes.to_vec();
  tokio::task::spawn_blocking(move || {
    on_progress(crate::RemoteInstallEvent::Extracting);
    let (manifest, files) = unpack_archive(&bytes)?;
    let layout = ctl_core::bundle_layout::Layout::managed(manifest.schema_version)
      .ok_or_else(|| invalid("unknown installation layout"))?;
    for &file_name in layout.names() {
      on_progress(crate::RemoteInstallEvent::Checking { file_name });
    }
    let stage = StagedBundle::new(&manifest, &files)?;
    on_progress(crate::RemoteInstallEvent::Activating);
    let installations = Installations::new(&home);
    let installed = installations.activate(&Bundle::open(&stage.0)?)?;
    // Cleanup cannot turn an already successful activation into an apparent
    // failed update. A later explicit cleanup can retry it safely.
    if let Err(error) = installations.prune() {
      eprintln!("ctlup: installed successfully; cleanup deferred: {error}");
    }
    on_progress(crate::RemoteInstallEvent::Complete);
    Ok(installed)
  })
  .await
  .map_err(io::Error::other)?
}

struct StagedBundle(PathBuf);

impl StagedBundle {
  fn new(manifest: &Manifest, files: &BTreeMap<String, Vec<u8>>) -> io::Result<Self> {
    // The installation transaction performs the complete account/path checks.
    // Stage in the OS temporary directory so an absent account root need not be
    // provisioned by a second implementation of those checks.
    let path = std::env::temp_dir().join(format!("ctlup-stage-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    let stage = Self(path);
    for (name, bytes) in files {
      let destination = stage.0.join(name);
      if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
      }
      let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)?;
      file.write_all(bytes)?;
      file.set_permissions(std::fs::Permissions::from_mode(
        if manifest.files[name].executable {
          0o700
        } else {
          0o600
        },
      ))?;
      file.sync_all()?;
    }
    std::fs::write(
      stage.0.join(MANIFEST_FILE),
      serde_json::to_vec(manifest).map_err(io::Error::other)?,
    )?;
    std::fs::set_permissions(
      stage.0.join(MANIFEST_FILE),
      std::fs::Permissions::from_mode(0o600),
    )?;
    Ok(stage)
  }
}

impl Drop for StagedBundle {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
  schema_version: u16,
  channel: BundleChannel,
}

/// The account's explicitly chosen channel, or the compiled distribution default.
///
/// # Errors
/// Reports invalid or unsafe account settings rather than silently replacing them.
pub fn default_channel(home: &Path) -> io::Result<BundleChannel> {
  if !storage_exists(&home.join(".tokn/ctl"))? {
    return Ok(BundleChannel::default());
  }
  let path = home.join(".tokn/ctl/ctlup.json");
  let bytes = match crate::components::read_input(&path, 4096) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BundleChannel::default()),
    Err(error) => return Err(error),
  };
  let settings: Settings = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if settings.schema_version != 1 {
    return Err(invalid("unsupported ctlup settings version"));
  }
  Ok(settings.channel)
}

/// Save the account's update channel after a successful explicit update.
///
/// # Errors
/// Reports unsafe storage or failed atomic settings publication.
pub fn set_channel(home: &Path, channel: BundleChannel) -> io::Result<()> {
  let root = home.join(".tokn/ctl");
  safe_directory(&root)?;
  let path = root.join(format!(".ctlup-settings-{}", uuid::Uuid::new_v4()));
  let result = (|| {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&path)?;
    serde_json::to_writer(
      &mut file,
      &Settings {
        schema_version: 1,
        channel,
      },
    )?;
    file.sync_all()?;
    std::fs::rename(&path, root.join("ctlup.json"))?;
    std::fs::File::open(&root)?.sync_all()
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(&path);
  }
  result
}

pub(super) fn safe_directory(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::MetadataExt as _;
  let home = path
    .ancestors()
    .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".tokn"))
    .and_then(Path::parent)
    .ok_or_else(|| invalid("ctlup storage must be inside the account directory"))?;
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
        if !metadata.is_dir()
          || metadata.uid() != rustix::process::getuid().as_raw()
          || metadata.mode() & 0o022 != 0
        {
          return Err(invalid(
            "ctlup storage must be owned directories, not links or writable by others",
          ));
        }
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
          Ok(()) => {}
          Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&directory)?;
            if !metadata.is_dir()
              || metadata.uid() != rustix::process::getuid().as_raw()
              || metadata.mode() & 0o022 != 0
            {
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
  use std::os::unix::fs::MetadataExt as _;
  let home = path
    .ancestors()
    .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".tokn"))
    .and_then(Path::parent)
    .ok_or_else(|| invalid("ctlup storage must be inside the account directory"))?;
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
    if !metadata.is_dir()
      || metadata.uid() != rustix::process::getuid().as_raw()
      || metadata.mode() & 0o022 != 0
    {
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
