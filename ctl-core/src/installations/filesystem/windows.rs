use super::{
  Bundle, Installations, JOURNAL, Lock, Temporary, Transaction, directory, ensure_directory,
  ensure_root, invalid, load, path_exists, private_file, read, valid_id, write,
};
use crate::bundle_layout::{ALL, MAX_FILE_BYTES, executable_name};
use crate::bundles::{digest, valid_digest};
use crate::installations::leases::{self, Lease};
use crate::private_fs::options;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

const LAUNCHER: &str = ".ctl-launcher.exe";
const MANAGER: &str = "ctlup.exe";
const MANAGER_STORE: &str = "updaters";

pub(in crate::installations) fn prepare_activation(store: &Installations) -> io::Result<()> {
  prepare_aliases(store, None)
}

fn selection(store: &Installations) -> io::Result<Option<Transaction>> {
  let path = store.root.join(JOURNAL);
  let bytes = match read(&path) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let transaction: Transaction = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if !valid_id(&transaction.active)
    || transaction
      .previous
      .as_deref()
      .is_some_and(|id| id == transaction.active || !valid_id(id))
  {
    return Err(invalid("invalid installation selection"));
  }
  Ok(Some(transaction))
}

pub(in crate::installations) fn pointer(
  store: &Installations,
  name: &str,
) -> io::Result<Option<Bundle>> {
  let Some(selection) = selection(store)? else {
    return Ok(None);
  };
  let id = if name == "current" {
    Some(selection.active)
  } else {
    selection.previous
  };
  id.as_deref()
    .map(|id| {
      let bundle = load(store, id)?;
      super::validate_native(&bundle)?;
      Ok(bundle)
    })
    .transpose()
}

pub(in crate::installations) fn activate(
  store: &Installations,
  active: &Bundle,
  previous: Option<&Bundle>,
) -> io::Result<()> {
  super::validate_native(active)?;
  if let Some(previous) = previous {
    super::validate_native(previous)?;
  }
  prepare_aliases(store, None)?;
  let active = &active.manifest.bundle_id;
  let transaction = Transaction {
    active: active.clone(),
    previous: previous
      .map(|bundle| &bundle.manifest.bundle_id)
      .filter(|id| *id != active)
      .cloned(),
  };
  if selection(store)?.is_some_and(|selection| {
    selection.active == transaction.active && selection.previous == transaction.previous
  }) {
    return Ok(());
  }
  // Both IDs are switched in one replacement. Existing processes use paths
  // inside their leased version and the stable launchers are never replaced.
  replace(
    &store.root.join(JOURNAL),
    &serde_json::to_vec(&transaction).map_err(io::Error::other)?,
    false,
  )
}

pub(in crate::installations) fn clear_previous(store: &Installations) -> io::Result<()> {
  if let Some(mut selection) = selection(store)?
    && selection.previous.take().is_some()
  {
    replace(
      &store.root.join(JOURNAL),
      &serde_json::to_vec(&selection).map_err(io::Error::other)?,
      false,
    )?;
  }
  Ok(())
}

pub(in crate::installations) fn install_manager(
  store: &Installations,
  updater: &[u8],
) -> io::Result<()> {
  if updater.is_empty() || updater.len() > MAX_FILE_BYTES {
    return Err(invalid("invalid updater executable size"));
  }
  ensure_root(store)?;
  let _lock = Lock::acquire(store)?;
  let root = store.root.join(MANAGER_STORE);
  ensure_directory(&root)?;
  let current = selected_manager(&root)?;
  let id = digest(updater);
  let destination = root.join(&id);
  if path_exists(&destination)? {
    directory(&destination)?;
    if executable_bytes(&destination.join(MANAGER))? != updater {
      return Err(invalid("stored updater failed payload verification"));
    }
  } else {
    let stage = Temporary(root.join(format!(".partial-{}", uuid::Uuid::new_v4())));
    ensure_directory(&stage.0)?;
    write(&stage.0.join(MANAGER), updater, true)?;
    fs::rename(&stage.0, destination)?;
  }
  prepare_aliases(store, Some(updater))?;
  if current.as_deref() != Some(&id) {
    replace(&root.join("active"), id.as_bytes(), false)?;
  }
  Ok(())
}

fn selected_manager(root: &Path) -> io::Result<Option<String>> {
  if !path_exists(root)? {
    return Ok(None);
  }
  directory(root)?;
  let bytes = match read(&root.join("active")) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let id = String::from_utf8(bytes).map_err(io::Error::other)?;
  if !valid_digest(&id) {
    return Err(invalid("invalid updater selection"));
  }
  Ok(Some(id))
}

pub(in crate::installations) fn manager_and_lease(
  store: &Installations,
) -> io::Result<Option<(PathBuf, Lease)>> {
  if !super::root_exists(store)? {
    return Ok(None);
  }
  let _lock = Lock::acquire(store)?;
  let root = store.root.join(MANAGER_STORE);
  let Some(id) = selected_manager(&root)? else {
    return Ok(None);
  };
  let path = verified_manager(&root, &id)?;
  let lease = leases::retain(&root.join("leases"), &id)?;
  Ok(Some((path, lease)))
}

pub(in crate::installations) fn lease_manager_executable(
  executable: &Path,
) -> io::Result<Option<Lease>> {
  if !executable.ends_with(MANAGER) {
    return Ok(None);
  }
  let Some(payload) = executable.parent() else {
    return Ok(None);
  };
  let Some(id) = payload
    .file_name()
    .and_then(|name| name.to_str())
    .filter(|id| valid_digest(id))
  else {
    return Ok(None);
  };
  let Some(root) = payload
    .parent()
    .filter(|parent| parent.ends_with(MANAGER_STORE))
  else {
    return Ok(None);
  };
  let Some(store) = root.parent().and_then(Installations::from_executable_root) else {
    return Ok(None);
  };
  if !super::root_exists(&store)? {
    return Err(invalid("managed updater has no installation store"));
  }
  let _lock = Lock::acquire(&store)?;
  verified_manager(root, id)?;
  leases::retain(&root.join("leases"), id).map(Some)
}

fn verified_manager(root: &Path, id: &str) -> io::Result<PathBuf> {
  directory(root)?;
  let directory_path = root.join(id);
  directory(&directory_path)?;
  let path = directory_path.join(MANAGER);
  if digest(&executable_bytes(&path)?) != id {
    return Err(invalid("stored updater failed payload verification"));
  }
  Ok(path)
}

pub(in crate::installations) fn prune_managers(store: &Installations) -> io::Result<()> {
  let root = store.root.join(MANAGER_STORE);
  let selected = selected_manager(&root)?;
  if !path_exists(&root)? {
    return Ok(());
  }
  for entry in fs::read_dir(&root)? {
    let entry = entry?;
    let name = entry.file_name();
    let Some(name) = name.to_str() else {
      continue;
    };
    if name
      .strip_prefix(".partial-")
      .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
      || (valid_digest(name)
        && selected.as_deref() != Some(name)
        && !leases::in_use_at(&root.join("leases"), name)?)
    {
      super::remove_installation(&entry.path())?;
    }
  }
  clean_replacements(&root)?;
  clean_replacements(&store.root)?;
  clean_replacements(&store.root.join("bin"))
}

fn clean_replacements(directory_path: &Path) -> io::Result<()> {
  if !path_exists(directory_path)? {
    return Ok(());
  }
  directory(directory_path)?;
  for entry in fs::read_dir(directory_path)? {
    let entry = entry?;
    if entry.file_name().to_str().is_some_and(|name| {
      name
        .strip_prefix(".ctlup-next-")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
    }) {
      private_file(&fs::symlink_metadata(entry.path())?)?;
      fs::remove_file(entry.path())?;
    }
  }
  Ok(())
}

fn prepare_aliases(store: &Installations, updater: Option<&[u8]>) -> io::Result<()> {
  let bin = store.root.join("bin");
  ensure_directory(&bin)?;
  let launcher = bin.join(LAUNCHER);
  let exists = path_exists(&launcher)?;
  let bytes;
  let source = if exists {
    bytes = executable_bytes(&launcher)?;
    &bytes
  } else {
    updater.ok_or_else(|| invalid("provision ctlup before activating an installation"))?
  };
  let mut missing = Vec::new();
  for name in ALL.into_iter().chain(["ctlup"]) {
    let alias = bin.join(executable_name(crate::paths::native_target(), name).as_ref());
    if path_exists(&alias)? {
      if executable_bytes(&alias)? != source {
        return Err(invalid(
          "stable command path already belongs to another installation",
        ));
      }
    } else {
      missing.push(alias);
    }
  }
  if !exists {
    replace(&launcher, source, true)?;
  }
  for alias in missing {
    replace(&alias, source, true)?;
  }
  Ok(())
}

fn executable_bytes(path: &Path) -> io::Result<Vec<u8>> {
  private_file(&fs::symlink_metadata(path)?)?;
  let file = options().read(true).open(path)?;
  private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file
    .take(MAX_FILE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)?;
  if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
    return Err(invalid("invalid updater or launcher executable size"));
  }
  Ok(bytes)
}

fn replace(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
  let next = Temporary(path.with_file_name(format!(".ctlup-next-{}", uuid::Uuid::new_v4())));
  write(&next.0, bytes, executable)?;
  fs::rename(&next.0, path)
}
