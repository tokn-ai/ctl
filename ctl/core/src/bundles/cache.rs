//! Bounded deployment payloads. Live users hold shared locks outside the payload.

use super::filesystem::{self, Lock};
use super::{Store, invalid, valid_digest, validate_target};
use crate::private_fs::{options, sync_directory};
use serde::Serialize;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PruneCacheResult {
  pub removed: Vec<String>,
  pub retained_in_use: Vec<String>,
  pub remaining_bytes: u64,
}

fn lease_path(store: &Store, target: &str, id: &str) -> PathBuf {
  store
    .root
    .join("leases")
    .join(target)
    .join(format!("{id}.lock"))
}

pub(super) fn prepare_lease(store: &Store, target: &str, id: &str) -> io::Result<()> {
  if !store.cache {
    return Ok(());
  }
  filesystem::ensure_directory(&store.root.join("leases"))?;
  filesystem::ensure_directory(&store.root.join("leases").join(target))?;
  let file = options()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(lease_path(store, target, id))?;
  filesystem::require_file(&file.metadata()?)?;
  file.set_modified(SystemTime::now())
}

/// Readers coordinate with GC before opening an existing entry. This never
/// creates lock files: inventory inspection remains read-only.
pub(super) fn read_guard(store: &Store) -> io::Result<Option<File>> {
  if !store.cache {
    return Ok(None);
  }
  let file = options().read(true).open(store.root.join("bundle.lock"))?;
  filesystem::require_file(&file.metadata()?)?;
  file.lock_shared()?;
  Ok(Some(file))
}

pub(super) fn lease(store: &Store, target: &str, id: &str) -> io::Result<Option<Arc<File>>> {
  if !store.cache {
    return Ok(None);
  }
  filesystem::require_directory(&store.root.join("leases"))?;
  filesystem::require_directory(&store.root.join("leases").join(target))?;
  let file = match options().read(true).open(lease_path(store, target, id)) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      return Err(invalid("cached component payload lost its retention lease"));
    }
    Err(error) => return Err(error),
  };
  filesystem::require_file(&file.metadata()?)?;
  file.lock_shared()?;
  Ok(Some(Arc::new(file)))
}

pub(super) fn touch(store: &Store, target: &str, id: &str) -> io::Result<()> {
  if !store.cache {
    return Ok(());
  }
  let file = options().write(true).open(lease_path(store, target, id))?;
  filesystem::require_file(&file.metadata()?)?;
  file.set_modified(SystemTime::now())
}

struct Candidate {
  id: String,
  path: PathBuf,
  lease: PathBuf,
  last_used: SystemTime,
  bytes: u64,
}

pub(super) fn prune(store: &Store, max_bytes: u64) -> io::Result<PruneCacheResult> {
  if !store.cache {
    return Err(invalid("only deployment caches can be pruned"));
  }
  if !filesystem::root_exists(store)? {
    return Ok(PruneCacheResult::default());
  }
  let _lock = Lock::acquire(&store.root.join("bundle.lock"))?;
  let mut candidates = candidates(store)?;
  candidates.sort_by(|left, right| {
    left
      .last_used
      .cmp(&right.last_used)
      .then(left.id.cmp(&right.id))
  });
  let mut result = PruneCacheResult {
    remaining_bytes: candidates.iter().map(|entry| entry.bytes).sum(),
    ..PruneCacheResult::default()
  };
  for entry in candidates {
    if result.remaining_bytes <= max_bytes {
      break;
    }
    let file = options().read(true).write(true).open(&entry.lease)?;
    filesystem::require_file(&file.metadata()?)?;
    match file.try_lock() {
      Ok(()) => {
        fs::remove_dir_all(&entry.path)?;
        sync_directory(entry.path.parent().unwrap())?;
        drop(file);
        fs::remove_file(&entry.lease)?;
        result.remaining_bytes = result.remaining_bytes.saturating_sub(entry.bytes);
        result.removed.push(entry.id);
      }
      Err(fs::TryLockError::WouldBlock) => result.retained_in_use.push(entry.id),
      Err(fs::TryLockError::Error(error)) => return Err(error),
    }
  }
  Ok(result)
}

fn candidates(store: &Store) -> io::Result<Vec<Candidate>> {
  let root = store.root.join("bundles");
  if !filesystem::directory_exists(&root)? {
    return Ok(Vec::new());
  }
  let mut candidates = Vec::new();
  for target in fs::read_dir(&root)? {
    let target = target?;
    filesystem::require_directory(&target.path())?;
    let target_name = target
      .file_name()
      .to_str()
      .map(str::to_owned)
      .ok_or_else(|| invalid("invalid cached target"))?;
    validate_target(&target_name)?;
    filesystem::require_directory(&store.root.join("leases"))?;
    filesystem::require_directory(&store.root.join("leases").join(&target_name))?;
    for entry in fs::read_dir(target.path())? {
      let entry = entry?;
      let name = entry.file_name();
      if name
        .to_str()
        .and_then(|name| name.strip_prefix(".partial-"))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
      {
        tree_bytes(&entry.path(), 0)?;
        fs::remove_dir_all(entry.path())?;
        continue;
      }
      let Some(id) = entry
        .file_name()
        .to_str()
        .filter(|id| valid_digest(id))
        .map(str::to_owned)
      else {
        continue;
      };
      let path = entry.path();
      filesystem::require_directory(&path)?;
      let lease = lease_path(store, &target_name, &id);
      let metadata = fs::symlink_metadata(&lease)?;
      filesystem::require_file(&metadata)?;
      let bytes = tree_bytes(&path, 0)?;
      candidates.push(Candidate {
        id,
        path,
        lease,
        last_used: metadata.modified()?,
        bytes,
      });
    }
  }
  Ok(candidates)
}

fn tree_bytes(path: &Path, depth: usize) -> io::Result<u64> {
  if depth > 16 {
    return Err(invalid("cached component payload has excessive nesting"));
  }
  filesystem::require_directory(path)?;
  let mut bytes = 0u64;
  for entry in fs::read_dir(path)? {
    let entry = entry?;
    let metadata = fs::symlink_metadata(entry.path())?;
    if metadata.is_dir() {
      bytes = bytes.saturating_add(tree_bytes(&entry.path(), depth + 1)?);
    } else {
      filesystem::require_file(&metadata)?;
      bytes = bytes.saturating_add(metadata.len());
    }
  }
  Ok(bytes)
}
