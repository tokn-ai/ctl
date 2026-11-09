use super::Installations;
use super::filesystem::{self, Lock};
use crate::bundles::Bundle;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// A process-held file lease, automatically released on exit or crash.
/// Keep the value alive for as long as the installation may be used.
#[derive(Debug)]
pub struct Lease {
  file: File,
  path: PathBuf,
}

impl Drop for Lease {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.path);
    let _ = self.file.unlock();
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
  retain_verified(store, &installed.manifest.bundle_id)
}

// The caller holds the installation lock from verification until publication.
fn retain_verified(store: &Installations, id: &str) -> io::Result<Lease> {
  let root = store.root.join("leases");
  filesystem::ensure_directory(&root)?;
  let root = root.join(id);
  // Abrupt exits bypass Drop. Sweep unlocked rows on the next launch, even
  // while this build remains active and is therefore excluded from payload GC.
  in_use(store, id)?;
  filesystem::ensure_directory(&root)?;
  let path = root.join(uuid::Uuid::new_v4().to_string());
  let file = filesystem::options()
    .read(true)
    .write(true)
    .create_new(true)
    .open(&path)?;
  file.lock()?;
  Ok(Lease { file, path })
}

pub(super) fn in_use(store: &Installations, id: &str) -> io::Result<bool> {
  let root = store.root.join("leases");
  if !filesystem::path_exists(&root)? {
    return Ok(false);
  }
  filesystem::directory(&root)?;
  let root = root.join(id);
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
    let file = match filesystem::options().read(true).write(true).open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error),
    };
    filesystem::private_file(&file.metadata()?)?;
    match file.try_lock() {
      Ok(()) => match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
      },
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
    let Some(root) = installs
      .parent()
      .filter(|parent| parent.ends_with(".tokn/ctl"))
    else {
      continue;
    };
    let home = root
      .parent()
      .and_then(std::path::Path::parent)
      .ok_or_else(|| filesystem::invalid("managed executable has no account directory"))?;
    let store = Installations::new(home);
    filesystem::ensure_root(&store)?;
    let _lock = Lock::acquire(&store)?;
    let bundle = store.get(id)?;
    return retain_verified(&store, &bundle.manifest.bundle_id).map(Some);
  }
  Ok(None)
}
