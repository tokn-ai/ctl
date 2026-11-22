use super::filesystem::{self, Lock};
use super::{Installations, catalog};
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
  let installed = store.get_recorded(&bundle.manifest.bundle_id)?;
  if installed.directory != bundle.directory || installed.manifest != bundle.manifest {
    return Err(filesystem::invalid(
      "process lease must pin this immutable installation",
    ));
  }
  retain(&store.runtime_leases(), &installed.manifest.bundle_id)
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
  in_use_at(&store.runtime_leases(), id)
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
  let lease = lease_executable(&executable)?;
  #[cfg(target_os = "linux")]
  if lease.is_none()
    && let Some(image) = std::env::var_os("APPIMAGE")
    && let Ok(image) = PathBuf::from(image).canonicalize()
    && image.ends_with("ctmux.AppImage")
    && image
      .parent()
      .and_then(Path::parent)
      .is_some_and(|parent| parent.ends_with("ctlup/store/desktop"))
  {
    // AppImage executes the GUI from its private mount. APPIMAGE records the
    // outer file whose managed payload must stay available for that process.
    return lease_executable(&image);
  }
  Ok(lease)
}

pub(super) fn lease_executable(executable: &Path) -> io::Result<Option<Lease>> {
  for directory in executable.ancestors().skip(1) {
    let Some(parent) = directory.parent() else {
      continue;
    };
    if parent.ends_with("ctlup/store/updater") {
      let relative = directory
        .strip_prefix(parent)
        .expect("installed updater descendant");
      return lease_manager(executable, parent, relative);
    }
    if let Some(package_root) = parent.parent()
      && (ends_in_target(directory) || ends_in_target(parent))
    {
      let relative = directory
        .strip_prefix(package_root)
        .expect("installed payload descendant");
      if package_root.ends_with("ctlup/store/runtime") {
        return lease_runtime(directory, package_root, relative);
      }
      if package_root.ends_with("ctlup/store/updater") {
        return lease_manager(executable, package_root, relative);
      }
    }
    let Some(id) = directory
      .file_name()
      .and_then(|name| name.to_str())
      .filter(|id| filesystem::valid_id(id))
    else {
      continue;
    };
    let package_root = parent;
    let Some(package) = package_root
      .file_name()
      .and_then(|name| name.to_str())
      .filter(|name| matches!(*name, "runtime" | "desktop"))
    else {
      continue;
    };
    let Some(updater_root) = package_root
      .parent()
      .filter(|root| root.ends_with("ctlup/store"))
      .and_then(Path::parent)
    else {
      continue;
    };
    let Some(store) = updater_root
      .parent()
      .and_then(Installations::from_executable_root)
    else {
      continue;
    };
    if package == "runtime" {
      return lease_runtime(directory, package_root, Path::new(id));
    }
    if !filesystem::root_exists(&store)? {
      return Err(filesystem::invalid(
        "managed executable has no installation registry",
      ));
    }
    filesystem::directory(&updater_root.join("store"))?;
    let _lock = Lock::acquire(&store)?;
    filesystem::directory(package_root)?;
    filesystem::directory(directory)?;
    if !crate::private_fs::is_owned_file(&fs::symlink_metadata(executable)?) {
      return Err(filesystem::invalid("unsafe executable in desktop payload"));
    }
    return retain(&store.desktop_leases(), id).map(Some);
  }
  Ok(None)
}

fn ends_in_target(path: &Path) -> bool {
  path
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(|target| crate::bundles::validate_target(target).is_ok())
}

fn lease_runtime(
  directory: &Path,
  runtime_root: &Path,
  relative: &Path,
) -> io::Result<Option<Lease>> {
  let Some(store) = runtime_root
    .ancestors()
    .nth(3)
    .and_then(Installations::from_executable_root)
  else {
    return Ok(None);
  };
  if !filesystem::root_exists(&store)? {
    return Err(filesystem::invalid(
      "managed executable has no installation registry",
    ));
  }
  let _lock = Lock::acquire(&store)?;
  let state = catalog::read_index(&store)?;
  let id = payload_id(&state.runtime_paths, relative)
    .ok_or_else(|| filesystem::invalid("managed runtime path has no installation registration"))?;
  let entry = super::catalog_entry(&state, id)
    .ok_or_else(|| filesystem::invalid("managed runtime path has no installation registration"))?;
  let bundle = store
    .load_entry(&state, entry)?
    .ok_or_else(|| filesystem::missing_payload(id))?;
  if bundle.directory.canonicalize()? != directory {
    return Err(filesystem::invalid(
      "managed runtime executable does not match its payload",
    ));
  }
  retain(&store.runtime_leases(), id).map(Some)
}

fn lease_manager(
  executable: &Path,
  manager_root: &Path,
  relative: &Path,
) -> io::Result<Option<Lease>> {
  let Some(store) = manager_root
    .ancestors()
    .nth(3)
    .and_then(Installations::from_executable_root)
  else {
    return Ok(None);
  };
  if !filesystem::root_exists(&store)? {
    return Err(filesystem::invalid(
      "managed executable has no installation registry",
    ));
  }
  let _lock = Lock::acquire(&store)?;
  let state = catalog::read_index(&store)?;
  let id = payload_id(&state.updater_paths, relative)
    .ok_or_else(|| filesystem::invalid("managed updater path has no installation registration"))?;
  let (path, _) = filesystem::read_manager(&store, &state.updater_paths, id)?
    .ok_or_else(|| filesystem::invalid("managed updater payload is missing"))?;
  if path.canonicalize()? != executable {
    return Err(filesystem::invalid(
      "managed updater executable does not match its payload",
    ));
  }
  retain(&store.manager_leases(), id).map(Some)
}

fn payload_id<'a>(paths: &'a catalog::PayloadPaths, relative: &'a Path) -> Option<&'a str> {
  relative
    .to_str()
    .filter(|id| filesystem::valid_id(id))
    .or_else(|| {
      paths
        .locations()
        .find_map(|(id, location)| (Path::new(location) == relative).then_some(id.as_str()))
    })
}
