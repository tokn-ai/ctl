use super::Installations;
use super::filesystem::{self, Lock};
use crate::bundles::Bundle;
use crate::private_fs::options;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// A process-held file lease, automatically released on exit or crash.
/// Keep the value alive for as long as the installation may be used.
#[derive(Debug)]
pub struct Lease {
  // Close the lock before trying to delete the file on Windows.
  _file: File,
  _path: LeaseFile,
}

#[derive(Debug)]
struct LeaseFile(PathBuf);

impl Drop for LeaseFile {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
    // Only pruning, while holding the installation lock, removes an empty
    // lease directory. Removing it here could race another process between
    // checking that directory and publishing its own lease.
  }
}

pub(super) fn acquire(store: &Installations, bundle: &Bundle) -> io::Result<Lease> {
  filesystem::ensure_root(store)?;
  let _lock = Lock::acquire(store)?;
  acquire_locked(store, bundle)
}

pub(super) fn acquire_locked(store: &Installations, bundle: &Bundle) -> io::Result<Lease> {
  let installed = filesystem::load(store, &bundle.manifest.bundle_id)?;
  if installed.directory != bundle.directory || installed.manifest != bundle.manifest {
    return Err(filesystem::invalid(
      "process lease must pin this immutable installation",
    ));
  }
  retain(&store.root.join("leases"), &installed.manifest.bundle_id)
}

// The caller holds the installation lock from verification until publication.
pub(super) fn retain(leases: &Path, id: &str) -> io::Result<Lease> {
  filesystem::ensure_directory(leases)?;
  // Abrupt exits bypass Drop. Sweep unlocked rows on the next launch, even
  // while this build remains active and is therefore excluded from payload GC.
  in_use_at(leases, id)?;
  let root = leases.join(id);
  filesystem::ensure_directory(&root)?;
  let path = root.join(uuid::Uuid::new_v4().to_string());
  let file = options()
    .read(true)
    .write(true)
    .create_new(true)
    .open(&path)?;
  file.lock()?;
  Ok(Lease {
    _file: file,
    _path: LeaseFile(path),
  })
}

pub(super) fn in_use(store: &Installations, id: &str) -> io::Result<bool> {
  in_use_at(&store.root.join("leases"), id)
}

pub(super) fn in_use_at(leases: &Path, id: &str) -> io::Result<bool> {
  if !filesystem::path_exists(leases)? {
    return Ok(false);
  }
  filesystem::directory(leases)?;
  let root = leases.join(id);
  if !filesystem::path_exists(&root)? {
    return Ok(false);
  }
  filesystem::directory(&root)?;
  let mut live = false;
  for (index, entry) in fs::read_dir(&root)?.enumerate() {
    if index >= 4096 {
      return Err(filesystem::invalid("too many installation leases"));
    }
    let path = entry?.path();
    let file = match options().read(true).write(true).open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error),
    };
    filesystem::private_file(&file.metadata()?)?;
    match file.try_lock() {
      Ok(()) => {
        drop(file);
        match fs::remove_file(path) {
          Ok(()) => {}
          Err(error) if error.kind() == io::ErrorKind::NotFound => {}
          Err(error) => return Err(error),
        }
      }
      Err(fs::TryLockError::WouldBlock) => live = true,
      Err(fs::TryLockError::Error(error)) => return Err(error),
    }
  }
  if !live {
    fs::remove_dir(root)?;
  }
  Ok(live)
}

/// Leases the installation containing the running executable, when managed.
/// Ordinary Cargo, package-manager and development executions remain passive.
///
/// # Errors
/// A detected managed executable with damaged installation metadata fails
/// rather than running without retention protection.
pub fn lease_current_executable() -> io::Result<Option<Lease>> {
  let executable = std::env::current_exe()?.canonicalize()?;
  lease_executable(&executable)
}

pub(super) fn lease_executable(executable: &Path) -> io::Result<Option<Lease>> {
  #[cfg(windows)]
  if let Some(lease) = filesystem::lease_manager_executable(executable)? {
    return Ok(Some(lease));
  }
  for directory in executable.ancestors().skip(1) {
    let Some(id) = directory.file_name().and_then(|name| name.to_str()) else {
      continue;
    };
    if !filesystem::valid_id(id) {
      continue;
    }
    let Some(installs) = directory
      .parent()
      .filter(|parent| parent.ends_with("installs"))
    else {
      continue;
    };
    let Some(store) = installs
      .parent()
      .and_then(Installations::from_executable_root)
    else {
      continue;
    };
    store.migrate_once_if_needed()?;
    filesystem::ensure_root(&store)?;
    let _lock = Lock::acquire(&store)?;
    let bundle = store.get_recorded(id)?;
    return retain(&store.root.join("leases"), &bundle.manifest.bundle_id).map(Some);
  }
  Ok(None)
}
