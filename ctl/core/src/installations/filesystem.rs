use super::{Installations, catalog, leases};
use crate::bundles::{Bundle, MANIFEST_FILE, local_target};
use crate::private_fs::{self, options, sync_directory};
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::symlink;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::prepare_aliases;

#[cfg(unix)]
const LAUNCHER: &str = ".ctl-launcher";
#[cfg(windows)]
const LAUNCHER: &str = ".ctl-launcher.exe";
#[cfg(unix)]
const MANAGER: &str = "ctlup";
#[cfg(windows)]
const MANAGER: &str = "ctlup.exe";

pub(super) fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn valid_id(id: &str) -> bool {
  id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn path_exists(path: &Path) -> io::Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

pub(super) fn directory(path: &Path) -> io::Result<()> {
  let metadata = fs::symlink_metadata(path)?;
  if !private_fs::is_directory(&metadata) {
    return Err(invalid(
      "installation directories must be owned and not writable by others",
    ));
  }
  Ok(())
}

fn incomplete_payload(error: io::Error, id: &str) -> io::Error {
  if error.kind() == io::ErrorKind::NotFound {
    return io::Error::new(
      io::ErrorKind::InvalidData,
      format!(
        "recorded runtime bundle {} has an incomplete payload under ctlup/store/runtime/{id}",
        &id[..12]
      ),
    );
  }
  error
}

pub(super) fn ensure_directory(path: &Path) -> io::Result<()> {
  match private_fs::create_directory(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => directory(path),
    Err(error) => Err(error),
  }
}

pub(super) fn private_file(metadata: &Metadata) -> io::Result<()> {
  if !private_fs::is_private_file(metadata) {
    return Err(invalid(
      "installation files must be private owned regular files",
    ));
  }
  Ok(())
}

pub(super) fn root_exists(store: &Installations) -> io::Result<bool> {
  let state = store.state_root();
  let anchor = crate::paths::managed_storage_anchor(&state)?;
  for path in storage_directories(anchor, &state)? {
    if !path_exists(path)? {
      return Ok(false);
    }
    directory(path)?;
  }
  Ok(true)
}

pub(super) fn ensure_root(store: &Installations) -> io::Result<()> {
  let anchor = crate::paths::managed_storage_anchor(&store.root)?;
  directory(anchor)?;
  for path in storage_directories(anchor, &store.root)?
    .into_iter()
    .skip(1)
  {
    ensure_directory(path)?;
  }
  let updater = store.updater_root();
  let state = store.state_root();
  for path in [
    updater.clone(),
    updater.join("store"),
    store.runtime_root(),
    store.staging_root(),
    state.clone(),
    state.join("leases"),
  ] {
    ensure_directory(&path)?;
  }
  Ok(())
}

fn storage_directories<'a>(anchor: &Path, root: &'a Path) -> io::Result<Vec<&'a Path>> {
  let mut paths = Vec::new();
  for path in root.ancestors() {
    paths.push(path);
    if path == anchor {
      paths.reverse();
      return Ok(paths);
    }
  }
  Err(invalid("installation is outside its storage directory"))
}

pub(super) fn validate_native(bundle: &Bundle) -> io::Result<()> {
  validate_complete(bundle)?;
  if !local_target(&bundle.manifest.target_triple) {
    return Err(invalid("an installation must match this computer's target"));
  }
  Ok(())
}

pub(super) fn validate_complete(bundle: &Bundle) -> io::Result<()> {
  bundle.manifest.validate()?;
  if bundle.manifest.schema_version != 2 {
    return Err(invalid(
      "catalog components require a complete build including ctl and ctmux",
    ));
  }
  Ok(())
}

pub(super) fn missing_payload(id: &str) -> io::Error {
  io::Error::new(
    io::ErrorKind::NotFound,
    format!(
      "recorded runtime bundle {} is missing under ctlup/store/runtime/{id}",
      &id[..12]
    ),
  )
}

pub(super) fn load(store: &Installations, id: &str) -> io::Result<Bundle> {
  load_available(store, id)?.ok_or_else(|| missing_payload(id))
}

pub(super) fn directory_if_present(path: &Path) -> io::Result<bool> {
  match directory(path) {
    Ok(()) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

pub(super) fn load_available(store: &Installations, id: &str) -> io::Result<Option<Bundle>> {
  let Some(path) = payload_dir(store, id)? else {
    return Ok(None);
  };
  open_payload(&path, id).map(Some)
}

pub(super) fn payload_dir(store: &Installations, id: &str) -> io::Result<Option<PathBuf>> {
  if !valid_id(id) {
    return Err(invalid("invalid installation identity"));
  }
  let installs = store.runtime_root();
  let path = installs.join(id);
  if !directory_if_present(&store.updater_root().join("store"))?
    || !directory_if_present(&installs)?
    || !directory_if_present(&path)?
  {
    return Ok(None);
  }
  Ok(Some(path))
}

fn open_payload(path: &Path, id: &str) -> io::Result<Bundle> {
  let bundle = Bundle::open(path).map_err(|error| incomplete_payload(error, id))?;
  validate_complete(&bundle)?;
  if bundle.manifest.bundle_id != id {
    return Err(invalid(
      "installation payload identity differs from its path",
    ));
  }
  Ok(bundle)
}

pub(super) fn publish(
  store: &Installations,
  source: &Bundle,
  files: Option<&BTreeMap<String, Vec<u8>>>,
) -> io::Result<Bundle> {
  validate_complete(source)?;
  let root = store.runtime_root();
  let destination = root.join(&source.manifest.bundle_id);
  if path_exists(&destination)? {
    let installed = load(store, &source.manifest.bundle_id)?;
    if installed.manifest != source.manifest {
      return Err(invalid(
        "immutable installation already contains different metadata",
      ));
    }
    return Ok(installed);
  }
  let owned;
  let files = if let Some(files) = files {
    files
  } else {
    owned = source.read_files()?;
    &owned
  };
  source.manifest.verify_files(files)?;
  let stage = Temporary(
    store
      .staging_root()
      .join(format!("runtime-{}", uuid::Uuid::new_v4())),
  );
  ensure_directory(&stage.0)?;
  for (name, bytes) in files {
    let path = stage.0.join(name);
    let mut parent = stage.0.clone();
    for part in path.parent().unwrap().strip_prefix(&stage.0).unwrap() {
      parent.push(part);
      ensure_directory(&parent)?;
    }
    write(&path, bytes, source.manifest.files[name].executable)?;
  }
  write(
    &stage.0.join(MANIFEST_FILE),
    &serde_json::to_vec(&source.manifest).map_err(io::Error::other)?,
    false,
  )?;
  Bundle::open(&stage.0)?;
  sync_tree(&stage.0)?;
  fs::rename(&stage.0, &destination)?;
  sync_directory(&root)?;
  sync_directory(&store.staging_root())?;
  load(store, &source.manifest.bundle_id)
}

fn write(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
  let mut file = options().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  if executable {
    private_fs::set_executable(&file)?;
  }
  file.sync_all()
}

fn sync_tree(path: &Path) -> io::Result<()> {
  for entry in fs::read_dir(path)? {
    let entry = entry?;
    if entry.file_type()?.is_dir() {
      sync_tree(&entry.path())?;
    }
  }
  sync_directory(path)
}

#[cfg(unix)]
fn prepare_aliases(store: &Installations, updater: Option<&[u8]>) -> io::Result<()> {
  let bin = store.root.join("bin");
  ensure_directory(&bin)?;
  let launcher = bin.join(LAUNCHER);
  let has_launcher = path_exists(&launcher)?;
  if has_launcher {
    executable_bytes(&launcher)?;
  }
  let mut aliases = Vec::new();
  for name in crate::bundle_layout::ALL.into_iter().chain(["ctlup"]) {
    let path = bin.join(name);
    if path_exists(&path)? {
      if !fs::symlink_metadata(&path)?.is_symlink() || fs::read_link(&path)? != Path::new(LAUNCHER)
      {
        return Err(invalid(
          "stable command path already belongs to another installation",
        ));
      }
    } else {
      aliases.push(path);
    }
  }
  if let (false, Some(bytes)) = (has_launcher, updater) {
    replace_launcher(&launcher, bytes)?;
  }
  for alias in aliases {
    symlink(LAUNCHER, alias)?;
  }
  sync_directory(&bin)
}

pub(super) fn prepare_activation(store: &Installations) -> io::Result<()> {
  prepare_aliases(store, None)
}

pub(super) fn install_manager(store: &Installations, updater: &[u8]) -> io::Result<()> {
  if updater.is_empty() || updater.len() > crate::bundles::MAX_FILE_BYTES {
    return Err(invalid("invalid updater executable size"));
  }
  ensure_root(store)?;
  let _lock = Lock::acquire(store)?;
  let mut state = catalog::read_index(store)?;
  catalog::initialize(store, &state)?;
  let root = store.manager_root();
  ensure_directory(&root)?;
  let id = crate::bundles::digest(updater);
  let destination = root.join(&id);
  if path_exists(&destination)? {
    if verified_manager(&root, &id).is_err() {
      return Err(invalid("stored updater failed payload verification"));
    }
  } else {
    let temporary = Temporary(
      store
        .staging_root()
        .join(format!("updater-{}", uuid::Uuid::new_v4())),
    );
    ensure_directory(&temporary.0)?;
    write(&temporary.0.join(MANAGER), updater, true)?;
    sync_directory(&temporary.0)?;
    fs::rename(&temporary.0, destination)?;
    sync_directory(&root)?;
    sync_directory(&store.staging_root())?;
  }
  prepare_aliases(store, Some(updater))?;
  if state.updater.as_ref() != Some(&id) {
    state.updater = Some(id);
    catalog::commit_metadata(store, &state)?;
  }
  Ok(())
}

pub(super) fn manager_and_lease(
  store: &Installations,
) -> io::Result<Option<(PathBuf, leases::Lease)>> {
  if !root_exists(store)? {
    return Ok(None);
  }
  let _lock = Lock::acquire(store)?;
  let Some(id) = catalog::read_index(store)?.updater else {
    return Ok(None);
  };
  directory(&store.updater_root().join("store"))?;
  let path = verified_manager(&store.manager_root(), &id)?;
  let lease = leases::retain(&store.manager_leases(), &id)?;
  Ok(Some((path, lease)))
}

pub(super) fn verified_manager(root: &Path, id: &str) -> io::Result<PathBuf> {
  if !valid_id(id) {
    return Err(invalid("invalid updater identity"));
  }
  directory(root)?;
  let payload = root.join(id);
  directory(&payload)?;
  let path = payload.join(MANAGER);
  if crate::bundles::digest(&executable_bytes(&path)?) != id {
    return Err(invalid("stored updater failed payload verification"));
  }
  Ok(path)
}

fn prune_package(root: &Path, leases_root: &Path, active: Option<&str>) -> io::Result<()> {
  if !directory_if_present(root)? {
    return Ok(());
  }
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let name = entry.file_name();
    if let Some(id) = name.to_str().filter(|id| valid_id(id))
      && active != Some(id)
      && !leases::in_use_at(leases_root, id)?
    {
      remove_installation(&entry.path())?;
    }
  }
  Ok(())
}

pub(super) fn prune_managers(store: &Installations, state: &catalog::State) -> io::Result<()> {
  if !directory_if_present(&store.updater_root().join("store"))? {
    return Ok(());
  }
  prune_package(
    &store.manager_root(),
    &store.manager_leases(),
    state.updater.as_deref(),
  )
}

pub(super) fn prune_desktops(store: &Installations, state: &catalog::State) -> io::Result<()> {
  if !directory_if_present(&store.updater_root().join("store"))? {
    return Ok(());
  }
  prune_package(
    &store.desktop_root(),
    &store.desktop_leases(),
    state
      .desktop
      .as_ref()
      .map(|selection| selection.content_id.as_str()),
  )
}

pub(super) fn desktop_available(store: &Installations, id: &str) -> io::Result<bool> {
  let desktop = store.desktop_root();
  Ok(
    directory_if_present(&store.updater_root().join("store"))?
      && directory_if_present(&desktop)?
      && directory_if_present(&desktop.join(id))?,
  )
}

fn replace_launcher(path: &Path, bytes: &[u8]) -> io::Result<()> {
  let next = Temporary(path.with_file_name(format!(".launcher-{}", uuid::Uuid::new_v4())));
  write(&next.0, bytes, true)?;
  fs::rename(&next.0, path)?;
  sync_directory(path.parent().unwrap())
}

fn executable_bytes(path: &Path) -> io::Result<Vec<u8>> {
  private_file(&fs::symlink_metadata(path)?)?;
  let file = options().read(true).open(path)?;
  private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file
    .take(crate::bundles::MAX_FILE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)?;
  if bytes.is_empty() || bytes.len() > crate::bundles::MAX_FILE_BYTES {
    return Err(invalid("invalid updater or launcher executable size"));
  }
  Ok(bytes)
}

pub(super) fn entries(store: &Installations) -> io::Result<Vec<(String, PathBuf)>> {
  let root = store.runtime_root();
  if !directory_if_present(&store.updater_root().join("store"))? || !path_exists(&root)? {
    return Ok(Vec::new());
  }
  directory(&root)?;
  let mut entries = Vec::new();
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let id = entry
      .file_name()
      .to_str()
      .map(str::to_owned)
      .unwrap_or_default();
    if valid_id(&id) {
      directory(&entry.path())?;
      entries.push((id, entry.path()));
    }
  }
  entries.sort_by(|left, right| left.0.cmp(&right.0));
  Ok(entries)
}

pub(super) fn has_payloads(store: &Installations) -> io::Result<bool> {
  if !directory_if_present(&store.updater_root().join("store"))? {
    return Ok(false);
  }
  for root in [
    store.runtime_root(),
    store.manager_root(),
    store.desktop_root(),
  ] {
    if directory_if_present(&root)? && fs::read_dir(&root)?.next().transpose()?.is_some() {
      return Ok(true);
    }
  }
  Ok(false)
}

pub(super) fn clean_staging(store: &Installations) -> io::Result<()> {
  let root = store.staging_root();
  if !directory_if_present(&root)? {
    return Ok(());
  }
  for entry in fs::read_dir(&root)? {
    let entry = entry?;
    let abandoned = entry
      .file_name()
      .to_str()
      .and_then(|name| {
        name
          .strip_prefix("runtime-")
          .or_else(|| name.strip_prefix("updater-"))
      })
      .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    if abandoned {
      directory(&entry.path())?;
      fs::remove_dir_all(entry.path())?;
    }
  }
  sync_directory(&root)
}

pub(super) fn remove_installation(path: &Path) -> io::Result<()> {
  directory(path)?;
  fs::remove_dir_all(path)?;
  sync_directory(path.parent().unwrap())
}

pub(super) struct Lock(File);
impl Lock {
  pub(super) fn acquire(store: &Installations) -> io::Result<Self> {
    let file = options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(store.state_root().join("installations.lock"))?;
    private_file(&file.metadata()?)?;
    file.lock()?;
    Ok(Self(file))
  }
}
impl Drop for Lock {
  fn drop(&mut self) {
    let _ = self.0.unlock();
  }
}

pub(super) struct Temporary(pub(super) PathBuf);
impl Drop for Temporary {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
    let _ = fs::remove_dir_all(&self.0);
  }
}
