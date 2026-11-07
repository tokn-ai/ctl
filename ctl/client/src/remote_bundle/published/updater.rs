//! The updater is distributed separately so runtime rollback never replaces it.

use super::{BundleChannel, Error, Resolver};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
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
      || !super::super::SUPPORTED_TARGETS.contains(&target)
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
      || info.binary != format!("ctlup-{}-{target}", info.bundle_id)
    {
      return Err(Error::Invalid(
        "invalid updater build, target or binary identity".into(),
      ));
    }
    if info.bundle_id != info.app_version
      && info.bundle_id != format!("{}-dev.{}", info.app_version, &info.git_revision[..12])
    {
      return Err(Error::Invalid(
        "updater bundle ID differs from its source identity".into(),
      ));
    }
    Ok(info)
  }

  pub(crate) fn verify(&self, bytes: Vec<u8>) -> Result<VerifiedUpdater, Error> {
    if bytes.len() as u64 != self.size || format!("{:x}", Sha256::digest(&bytes)) != self.sha256 {
      return Err(Error::Invalid("updater size or checksum mismatch".into()));
    }
    Ok(VerifiedUpdater {
      info: self.clone(),
      bytes,
    })
  }
}

/// Read an explicitly supplied updater and its matching bounded sidecar.
///
/// # Errors
/// Rejects missing metadata, wrong targets, unsafe input files and corrupt bytes.
pub fn read_updater(directory: &Path, target: &str) -> Result<VerifiedUpdater, Error> {
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
  let publication = resolver.publication(channel).await?;
  let url = publication
    .url
    .join(&format!("ctlup-{target}.json"))
    .map_err(|error| Error::Invalid(error.to_string()))?;
  let metadata = resolver.read(&url, super::MAX_BUNDLE_SET_BYTES).await?;
  let info = UpdaterInfo::parse(&metadata, target)?;
  if info.app_version != publication.manifest.app_version
    || info.bundle_id != publication.manifest.bundle_id
    || info.git_revision != publication.manifest.git_revision
  {
    return Err(Error::Invalid(
      "updater differs from its immutable publication".into(),
    ));
  }
  if let Some(verified) = read_cached_updater(cache_root, &info)? {
    return Ok(verified);
  }
  let binary_url = publication
    .url
    .join(&info.binary)
    .map_err(|error| Error::Invalid(error.to_string()))?;
  let verified = info.verify(resolver.read(&binary_url, super::MAX_BUNDLE_BYTES).await?)?;
  let root = cache_root.to_owned();
  let snapshot = verified.clone();
  tokio::task::spawn_blocking(move || cache_verified(&root, &snapshot))
    .await
    .map_err(|error| Error::Io(io::Error::other(error)))??;
  Ok(verified)
}

pub(crate) fn cache_verified(root: &Path, updater: &VerifiedUpdater) -> Result<(), Error> {
  let encoded =
    serde_json::to_vec(&updater.info).map_err(|error| Error::Invalid(error.to_string()))?;
  let info = UpdaterInfo::parse(&encoded, &updater.info.target_triple)?;
  info.verify(updater.bytes.clone())?;
  crate::installation::safe_directory(root)?;
  let _lock = CacheLock::acquire(root)?;
  if read_cached_updater(root, &updater.info)?.is_some() {
    return Ok(());
  }
  let stage = root.join(format!(".updater-{}", uuid::Uuid::new_v4()));
  std::fs::DirBuilder::new().mode(0o700).create(&stage)?;
  let result = (|| {
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o700)
      .open(stage.join("ctlup"))?;
    file.write_all(&updater.bytes)?;
    file.sync_all()?;
    let destination = root.join(&updater.info.sha256);
    std::fs::File::open(&stage)?.sync_all()?;
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
    std::fs::File::open(root)?.sync_all()?;
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
  if !existing_cache_path(root)? {
    return Ok(0);
  }
  let _lock = CacheLock::acquire(root)?;
  let (mut blobs, stages) = cache_blobs(root)?;
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
  let stale = stale_records(root, &retained)?;
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
    std::fs::File::open(root.join("records"))?.sync_all()?;
  }
  std::fs::File::open(root)?.sync_all()?;
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

fn stale_records(
  root: &Path,
  retained: &std::collections::BTreeSet<&str>,
) -> io::Result<Vec<std::path::PathBuf>> {
  let directory = root.join("records");
  if !owned_directory(&directory)? {
    return Ok(Vec::new());
  }
  let mut stale = Vec::new();
  for entry in std::fs::read_dir(&directory)? {
    let entry = entry?;
    let name = entry.file_name();
    let name = name.to_str().ok_or_else(unsafe_cache)?;
    require_owned_file(&std::fs::symlink_metadata(entry.path())?)?;
    // Record writers publish by rename after writing a bounded private stage.
    // They do not hold the cache lock; leave an in-flight record stage alone.
    if name.starts_with(".record-") {
      continue;
    }
    if !name.strip_suffix(".json").is_some_and(valid_digest) {
      return Err(unsafe_cache());
    }
    let bytes = crate::components::read_input(&entry.path(), 16 * 1024)?;
    let info: UpdaterInfo = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let info = UpdaterInfo::parse(&bytes, &info.target_triple).map_err(io::Error::other)?;
    if !retained.contains(info.sha256.as_str()) {
      stale.push(entry.path());
    }
  }
  Ok(stale)
}

fn valid_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn existing_cache_path(path: &Path) -> io::Result<bool> {
  let home = path
    .ancestors()
    .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".tokn"))
    .and_then(Path::parent)
    .ok_or_else(unsafe_cache)?;
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
    Ok(metadata)
      if metadata.is_dir()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o022 == 0 =>
    {
      Ok(true)
    }
    Ok(_) => Err(unsafe_cache()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn require_owned_file(metadata: &std::fs::Metadata) -> io::Result<()> {
  if !metadata.is_file()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o022 != 0
  {
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
    let file = std::fs::File::from(
      rustix::fs::open(
        root.join("updater.lock"),
        rustix::fs::OFlags::CREATE
          | rustix::fs::OFlags::RDWR
          | rustix::fs::OFlags::NOFOLLOW
          | rustix::fs::OFlags::NONBLOCK
          | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
      )
      .map_err(io::Error::from)?,
    );
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

#[cfg(test)]
mod tests;
