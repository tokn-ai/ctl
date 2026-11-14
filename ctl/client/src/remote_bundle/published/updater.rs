//! The updater is distributed separately from runtime bundles.

use super::{AssetProgress, BundleChannel, Error, Publication, Resolver, report_progress};
use ctl_core::{bundle_layout, private_fs};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::{self, Write as _};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdaterInfo {
  pub schema_version: u16,
  pub app_version: String,
  pub bundle_id: String,
  pub git_revision: String,
  pub target_triple: String,
  pub binary: String,
  pub sha256: String,
  pub size: u64,
}

#[derive(Debug, Clone)]
pub struct VerifiedUpdater {
  pub info: UpdaterInfo,
  pub bytes: Vec<u8>,
}

impl UpdaterInfo {
  pub(crate) fn parse(bytes: &[u8], target: &str) -> Result<Self, Error> {
    if bytes.len() > 16 * 1024 {
      return Err(Error::Invalid(
        "updater metadata exceeds its size limit".into(),
      ));
    }
    let info: Self = serde_json::from_slice(bytes)
      .map_err(|_| Error::Invalid("invalid updater metadata".into()))?;
    if info.schema_version != 1
      || info.target_triple != target
      || !super::super::supported_target(target)
      || !super::super::safe_id(&info.app_version)
      || !super::super::safe_id(&info.bundle_id)
      || info.git_revision.len() != 40
      || !info
        .git_revision
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit())
      || info.sha256.len() != 64
      || !info.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
      || info.size == 0
      || info.size > super::MAX_BUNDLE_BYTES as u64
      || info.binary
        != bundle_layout::executable_name(target, &format!("ctlup-{}-{target}", info.bundle_id))
    {
      return Err(Error::Invalid(
        "invalid updater build, target or binary identity".into(),
      ));
    }
    if !super::super::valid_bundle_id(&info.app_version, &info.bundle_id, &info.git_revision) {
      return Err(Error::Invalid(
        "updater bundle ID differs from its source identity".into(),
      ));
    }
    Ok(info)
  }

  pub(crate) fn verify(&self, bytes: Vec<u8>) -> Result<VerifiedUpdater, Error> {
    self.verify_bytes(&bytes)?;
    Ok(VerifiedUpdater {
      info: self.clone(),
      bytes,
    })
  }

  fn verify_bytes(&self, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() as u64 != self.size || format!("{:x}", Sha256::digest(bytes)) != self.sha256 {
      return Err(Error::Invalid("updater size or checksum mismatch".into()));
    }
    Ok(())
  }
}

/// Read an explicitly supplied updater and its matching bounded sidecar.
///
/// # Errors
/// Rejects missing metadata, wrong targets, unsafe input files and corrupt bytes.
pub fn read_updater(directory: &Path, target: &str) -> Result<VerifiedUpdater, Error> {
  super::super::validate_target(target)?;
  let bytes = crate::components::read_input(
    &directory.join(format!("ctlup-{target}.json")),
    super::MAX_BUNDLE_SET_BYTES,
  )?;
  let info = UpdaterInfo::parse(&bytes, target)?;
  info.verify(crate::components::read_input(
    &directory.join(&info.binary),
    super::MAX_BUNDLE_BYTES,
  )?)
}

/// Resolve the account's channel and download its separate updater on a cache miss.
///
/// # Errors
/// Rejects unavailable publications, untrusted URLs or changed updater bytes.
pub async fn download_updater_cached(
  channel: BundleChannel,
  target: &str,
  cache_root: &Path,
) -> Result<VerifiedUpdater, Error> {
  let resolver = Resolver::production()?;
  let publication = resolver.target_publication(channel, target).await?;
  resolver
    .updater_cached(&publication, target, cache_root, None)
    .await
}

impl Resolver {
  pub(super) async fn updater_cached(
    &self,
    publication: &Publication,
    target: &str,
    cache_root: &Path,
    progress: AssetProgress<'_>,
  ) -> Result<VerifiedUpdater, Error> {
    report_progress(progress, 0, 0);
    let url = publication
      .url
      .join(&format!("ctlup-{target}.json"))
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let metadata = self
      .read(&url, 16 * 1024)
      .await
      .map_err(|error| missing_updater(error, publication, target, "metadata"))?;
    let info = UpdaterInfo::parse(&metadata, target)?;
    if info.app_version != publication.manifest.app_version
      || info.bundle_id != publication.manifest.bundle_id
      || info.git_revision != publication.manifest.git_revision
    {
      return Err(Error::Invalid(
        "updater differs from its immutable publication".into(),
      ));
    }
    let root = cache_root.to_owned();
    let read_root = root.clone();
    let read_info = info.clone();
    if let Some(verified) =
      tokio::task::spawn_blocking(move || read_cached_updater(&read_root, &read_info))
        .await
        .map_err(|error| Error::Io(io::Error::other(error)))??
    {
      report_progress(progress, info.size, info.size);
      return Ok(verified);
    }
    let binary_url = publication
      .url
      .join(&info.binary)
      .map_err(|error| Error::Invalid(error.to_string()))?;
    let maximum = usize::try_from(info.size)
      .map_err(|_| Error::Invalid("updater exceeds its size limit on this platform".into()))?;
    let bytes = self
      .read_with_progress(&binary_url, maximum, Some(info.size), progress)
      .await
      .map_err(|error| missing_updater(error, publication, target, "executable"))?;
    let verified = tokio::task::spawn_blocking(move || {
      let verified = info.verify(bytes)?;
      cache_verified(&root, &verified)?;
      Ok::<_, Error>(verified)
    })
    .await
    .map_err(|error| Error::Io(io::Error::other(error)))??;
    report_progress(progress, verified.info.size, verified.info.size);
    Ok(verified)
  }
}

fn missing_updater(error: Error, publication: &Publication, target: &str, asset: &str) -> Error {
  if matches!(error, Error::NotAvailable(_)) {
    return Error::NotAvailable(format!(
      "publication {} has no ctlup {asset} for {target}",
      publication.manifest.bundle_id,
    ));
  }
  error
}

pub(crate) fn cache_verified(root: &Path, updater: &VerifiedUpdater) -> Result<(), Error> {
  validate_updater(updater)?;
  crate::installation::safe_directory(root)?;
  let _lock = CacheLock::acquire(root)?;
  cache_verified_locked(root, updater)
}

fn validate_updater(updater: &VerifiedUpdater) -> Result<(), Error> {
  let encoded =
    serde_json::to_vec(&updater.info).map_err(|error| Error::Invalid(error.to_string()))?;
  let info = UpdaterInfo::parse(&encoded, &updater.info.target_triple)?;
  info.verify_bytes(&updater.bytes)?;
  Ok(())
}

pub(crate) fn validate_updater_for_manifest(
  manifest: &ctl_core::bundles::Manifest,
  updater: &VerifiedUpdater,
) -> Result<(), Error> {
  manifest.validate()?;
  validate_updater(updater)?;
  let build = &manifest.components["ctl-agent"].build;
  if updater.info.target_triple != manifest.target_triple
    || updater.info.app_version != build.version
    || Some(updater.info.git_revision.as_str()) != build.source_revision.as_deref()
    || manifest
      .distribution_id
      .as_ref()
      .is_some_and(|id| id != &updater.info.bundle_id)
  {
    return Err(Error::Invalid(
      "updater differs from its installed component publication".into(),
    ));
  }
  Ok(())
}

pub(crate) fn cache_verified_for_manifest(
  root: &Path,
  manifest: &ctl_core::bundles::Manifest,
  updater: &VerifiedUpdater,
) -> Result<(), Error> {
  validate_updater_for_manifest(manifest, updater)?;
  crate::installation::safe_directory(root)?;
  let _lock = CacheLock::acquire(root)?;
  cache_verified_locked(root, updater)?;
  let records = root.join("records");
  crate::installation::safe_directory(&records)?;
  let stage = records.join(format!(".record-{}", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = private_fs::options()
      .write(true)
      .create_new(true)
      .open(&stage)?;
    file.write_all(&serde_json::to_vec(&updater.info).map_err(io::Error::other)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&stage, records.join(format!("{}.json", manifest.bundle_id)))?;
    private_fs::sync_directory(&records)?;
    Ok::<_, Error>(())
  })();
  if result.is_err() {
    let _ = std::fs::remove_file(stage);
  }
  result
}

fn cache_verified_locked(root: &Path, updater: &VerifiedUpdater) -> Result<(), Error> {
  if read_cached_updater(root, &updater.info)?.is_some() {
    return Ok(());
  }
  let stage = root.join(format!(".updater-{}", uuid::Uuid::new_v4()));
  private_fs::create_directory(&stage)?;
  let result = (|| {
    let mut file = private_fs::options()
      .write(true)
      .create_new(true)
      .open(stage.join("ctlup"))?;
    file.write_all(&updater.bytes)?;
    private_fs::set_executable(&file)?;
    file.sync_all()?;
    drop(file);
    let destination = root.join(&updater.info.sha256);
    private_fs::sync_directory(&stage)?;
    let displaced = if owned_directory(&destination)? {
      let displaced = root.join(format!(".corrupt-updater-{}", uuid::Uuid::new_v4()));
      std::fs::rename(&destination, &displaced)?;
      Some(displaced)
    } else {
      None
    };
    if let Err(error) = std::fs::rename(&stage, &destination) {
      if let Some(displaced) = displaced {
        let _ = std::fs::rename(displaced, &destination);
      }
      return Err(Error::Io(error));
    }
    if let Some(displaced) = displaced {
      let _ = std::fs::remove_dir_all(displaced);
    }
    private_fs::sync_directory(root)?;
    Ok(())
  })();
  let _ = std::fs::remove_dir_all(stage);
  result
}

pub(crate) fn read_cached_updater(
  root: &Path,
  info: &UpdaterInfo,
) -> Result<Option<VerifiedUpdater>, Error> {
  if !existing_cache_path(root)? || !owned_directory(&root.join(&info.sha256))? {
    return Ok(None);
  }
  let binary = root.join(&info.sha256).join("ctlup");
  match std::fs::symlink_metadata(&binary) {
    Ok(metadata) => {
      require_owned_file(&metadata)?;
      if metadata.len() > super::MAX_BUNDLE_BYTES as u64 {
        return Ok(None);
      }
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(Error::Io(error)),
  }
  match crate::components::read_input(&binary, super::MAX_BUNDLE_BYTES) {
    Ok(bytes) => Ok(info.verify(bytes).ok()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(Error::Io(error)),
  }
}

/// Bound deployment-only updater blobs; active runtime installations are separate.
/// Writers and cleanup share one process-held lock, so interrupted writers never
/// leave a persistent lock directory that blocks a future update.
pub(crate) fn prune_updater_cache(root: &Path, maximum_bytes: u64) -> io::Result<u64> {
  prune_updater_cache_with_pins(root, maximum_bytes, &[])
}

pub(crate) fn prune_updater_cache_with_pins(
  root: &Path,
  maximum_bytes: u64,
  pins: &[ctl_core::bundles::Manifest],
) -> io::Result<u64> {
  if !existing_cache_path(root)? {
    return Ok(0);
  }
  let _lock = CacheLock::acquire(root)?;
  let (mut blobs, stages) = cache_blobs(root)?;
  let records = cache_records(root)?;
  let pinned = records
    .iter()
    .filter(|(path, info)| {
      pins.iter().any(|manifest| {
        path
          .file_stem()
          .is_some_and(|name| name == manifest.bundle_id.as_str())
          && manifest.target_triple == info.target_triple
          && manifest
            .distribution_id
            .as_ref()
            .is_none_or(|id| id == &info.bundle_id)
          && manifest
            .components
            .get("ctl-agent")
            .is_some_and(|component| {
              component.build.version == info.app_version
                && component.build.source_revision.as_deref() == Some(info.git_revision.as_str())
            })
      })
    })
    .map(|(_, info)| info.sha256.as_str())
    .collect::<std::collections::BTreeSet<_>>();
  blobs.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
  let mut total = blobs.iter().try_fold(0_u64, |total, blob| {
    total
      .checked_add(blob.2)
      .ok_or_else(|| io::Error::other("updater cache size overflow"))
  })?;
  let mut removed_ids = std::collections::BTreeSet::new();
  let mut removed_bytes = 0_u64;
  for (_, id, size) in &blobs {
    if total <= maximum_bytes {
      break;
    }
    if pinned.contains(id.as_str()) {
      continue;
    }
    removed_ids.insert(id.as_str());
    total -= size;
    removed_bytes += size;
  }
  let retained = blobs
    .iter()
    .filter(|(_, id, _)| !removed_ids.contains(id.as_str()))
    .map(|(_, id, _)| id.as_str())
    .collect::<std::collections::BTreeSet<_>>();
  // Validate all record inputs before the first mutation. A malformed record
  // cannot make cleanup silently follow or remove an unrelated path.
  let stale = records
    .iter()
    .filter(|(_, info)| !retained.contains(info.sha256.as_str()))
    .map(|(path, _)| path);
  for path in stages {
    std::fs::remove_dir_all(path)?;
  }
  for id in removed_ids {
    std::fs::remove_dir_all(root.join(id))?;
  }
  for path in stale {
    std::fs::remove_file(path)?;
  }
  if owned_directory(&root.join("records"))? {
    private_fs::sync_directory(&root.join("records"))?;
  }
  private_fs::sync_directory(root)?;
  Ok(removed_bytes)
}

type CacheBlob = (std::time::SystemTime, String, u64);

fn cache_blobs(root: &Path) -> io::Result<(Vec<CacheBlob>, Vec<std::path::PathBuf>)> {
  let mut blobs = Vec::new();
  let mut stages = Vec::new();
  for entry in std::fs::read_dir(root)? {
    let entry = entry?;
    let name = entry.file_name();
    let name = name.to_str().ok_or_else(unsafe_cache)?;
    if name == "updater.lock" {
      require_owned_file(&std::fs::symlink_metadata(entry.path())?)?;
    } else if name == "records" {
      owned_directory(&entry.path())?;
    } else if valid_digest(name) {
      owned_directory(&entry.path())?;
      let children = std::fs::read_dir(entry.path())?.collect::<Result<Vec<_>, _>>()?;
      if children.len() != 1 || children[0].file_name() != "ctlup" {
        return Err(unsafe_cache());
      }
      let binary = entry.path().join("ctlup");
      let metadata = std::fs::symlink_metadata(&binary)?;
      require_owned_file(&metadata)?;
      let bytes = crate::components::read_input(&binary, super::MAX_BUNDLE_BYTES)?;
      if format!("{:x}", Sha256::digest(&bytes)) != name {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "updater cache content differs from its digest",
        ));
      }
      blobs.push((metadata.modified()?, name.into(), metadata.len()));
    } else if name.starts_with(".updater-") || name.starts_with(".corrupt-updater-") {
      owned_directory(&entry.path())?;
      stages.push(entry.path());
    } else {
      return Err(unsafe_cache());
    }
  }
  Ok((blobs, stages))
}

fn cache_records(root: &Path) -> io::Result<Vec<(std::path::PathBuf, UpdaterInfo)>> {
  let directory = root.join("records");
  if !owned_directory(&directory)? {
    return Ok(Vec::new());
  }
  let mut records = Vec::new();
  for entry in std::fs::read_dir(&directory)? {
    let entry = entry?;
    let name = entry.file_name();
    let name = name.to_str().ok_or_else(unsafe_cache)?;
    require_owned_file(&std::fs::symlink_metadata(entry.path())?)?;
    // Records publish by rename after writing a bounded private stage.
    // Leave incomplete stages from older concurrent updater versions alone.
    if name.starts_with(".record-") {
      continue;
    }
    if !name.strip_suffix(".json").is_some_and(valid_digest) {
      return Err(unsafe_cache());
    }
    let bytes = crate::components::read_input(&entry.path(), 16 * 1024)?;
    let info: UpdaterInfo = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let info = UpdaterInfo::parse(&bytes, &info.target_triple).map_err(io::Error::other)?;
    records.push((entry.path(), info));
  }
  Ok(records)
}

fn valid_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn existing_cache_path(path: &Path) -> io::Result<bool> {
  let home = ctl_core::paths::managed_storage_anchor(path)?;
  let mut directory = home.to_owned();
  for part in std::iter::once(None).chain(
    path
      .strip_prefix(home)
      .map_err(|_| unsafe_cache())?
      .components()
      .map(Some),
  ) {
    if let Some(part) = part {
      if !matches!(part, std::path::Component::Normal(_)) {
        return Err(unsafe_cache());
      }
      directory.push(part);
    }
    if !owned_directory(&directory)? {
      return Ok(false);
    }
  }
  Ok(true)
}

fn owned_directory(path: &Path) -> io::Result<bool> {
  match std::fs::symlink_metadata(path) {
    Ok(metadata) if private_fs::is_directory(&metadata) => Ok(true),
    Ok(_) => Err(unsafe_cache()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn require_owned_file(metadata: &std::fs::Metadata) -> io::Result<()> {
  if !private_fs::is_owned_file(metadata) {
    return Err(unsafe_cache());
  }
  Ok(())
}

fn unsafe_cache() -> io::Error {
  io::Error::new(
    io::ErrorKind::PermissionDenied,
    "updater cache paths must be owned regular files and directories, not links or writable by others",
  )
}

struct CacheLock(std::fs::File);

impl CacheLock {
  fn acquire(root: &Path) -> io::Result<Self> {
    let file = private_fs::options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(root.join("updater.lock"))?;
    require_owned_file(&file.metadata()?)?;
    file.lock()?;
    Ok(Self(file))
  }
}

impl Drop for CacheLock {
  fn drop(&mut self) {
    let _ = self.0.unlock();
  }
}

#[cfg(all(test, unix))]
mod tests;
