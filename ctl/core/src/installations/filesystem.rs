use super::Installations;
use crate::bundles::{Bundle, MANIFEST_FILE, local_target};
use crate::private_fs::{self, options, sync_directory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt as _, symlink};
#[cfg(unix)]
use std::path::Component;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{
  activate, clear_previous, install_manager, lease_manager_executable, manager_and_lease, pointer,
  prepare_activation, prune_managers,
};

const JOURNAL: &str = ".activation.json";
#[cfg(unix)]
pub(super) const CTLD_PROXY: &[u8] = include_bytes!("ctld-proxy.sh");

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
  active: String,
  previous: Option<String>,
}

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
  let anchor = crate::paths::managed_storage_anchor(&store.root)?;
  for path in storage_directories(anchor, &store.root)? {
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
  ensure_directory(&store.root.join("installs"))
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

#[cfg(unix)]
fn pointer_path(store: &Installations, name: &str) -> io::Result<Option<PathBuf>> {
  let path = store.root.join(name);
  if !path_exists(&path)? {
    return Ok(None);
  }
  let link = fs::read_link(path)?;
  if link.is_absolute()
    || link
      .components()
      .any(|part| !matches!(part, Component::Normal(_)))
  {
    return Err(invalid(
      "installation pointer must be a managed relative link",
    ));
  }
  let parts = link.iter().collect::<Vec<_>>();
  let managed =
    parts.len() == 2 && parts[0] == "installs" && parts[1].to_str().is_some_and(valid_id);
  if !managed {
    return Err(invalid(
      "installation pointer is not a canonical managed selection",
    ));
  }
  let mut parent = store.root.clone();
  for part in parts {
    parent.push(part);
    directory(&parent)?;
  }
  Ok(Some(parent))
}

#[cfg(unix)]
pub(super) fn pointer(store: &Installations, name: &str) -> io::Result<Option<Bundle>> {
  let Some(path) = pointer_path(store, name)? else {
    return Ok(None);
  };
  let bundle = Bundle::open(&path)?;
  validate_native(&bundle)?;
  if path.file_name().and_then(|name| name.to_str()) != Some(&bundle.manifest.bundle_id) {
    return Err(invalid(
      "installation payload identity differs from its path",
    ));
  }
  Ok(Some(bundle))
}

pub(super) fn load(store: &Installations, id: &str) -> io::Result<Bundle> {
  if !valid_id(id) {
    return Err(invalid("invalid installation identity"));
  }
  directory(&store.root.join("installs"))?;
  let bundle = Bundle::open(&store.root.join("installs").join(id))?;
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
  let root = store.root.join("installs");
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
  let stage = Temporary(root.join(format!(".partial-{}", uuid::Uuid::new_v4())));
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

fn read(path: &Path) -> io::Result<Vec<u8>> {
  private_file(&fs::symlink_metadata(path)?)?;
  let file = options().read(true).open(path)?;
  private_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(1025).read_to_end(&mut bytes)?;
  if bytes.len() > 1024 {
    return Err(invalid("installation transaction is oversized"));
  }
  Ok(bytes)
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
fn replace_link(path: &Path, target: &Path) -> io::Result<()> {
  if path_exists(path)? && !fs::symlink_metadata(path)?.is_symlink() {
    return Err(invalid("refusing to replace a non-link installation entry"));
  }
  let next = Temporary(path.with_file_name(format!(".next-{}", uuid::Uuid::new_v4())));
  symlink(target, &next.0)?;
  fs::rename(&next.0, path)?;
  File::open(path.parent().unwrap())?.sync_all()
}

#[cfg(unix)]
fn existing_proxy(path: &Path) -> io::Result<bool> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_file() {
    return Ok(false);
  }
  private_file(&metadata)?;
  if metadata.permissions().mode() & 0o777 != 0o700 || read(path)? != CTLD_PROXY {
    return Err(invalid(
      "stable ctld command differs from the managed proxy",
    ));
  }
  Ok(true)
}

#[cfg(unix)]
fn prepare_proxy(root: &Path) -> io::Result<()> {
  let path = root.join("ctld");
  if path_exists(&path)? && existing_proxy(&path)? {
    return Ok(());
  }
  let next = Temporary(root.join(format!(".ctld-proxy-{}", uuid::Uuid::new_v4())));
  write(&next.0, CTLD_PROXY, true)?;
  fs::rename(&next.0, path)?;
  File::open(root)?.sync_all()
}

#[cfg(unix)]
fn prepare_aliases(store: &Installations) -> io::Result<()> {
  let root = store.root.join("bin");
  ensure_directory(&root)?;
  for name in crate::bundle_layout::ALL {
    let path = root.join(name);
    if path_exists(&path)? {
      if name == "ctld" && existing_proxy(&path)? {
        continue;
      }
      let target = fs::read_link(&path)?;
      let expected = Path::new("../current").join(name);
      let signed = Path::new("../current/ctld.app/Contents/MacOS/ctld");
      if target != expected && !(name == "ctld" && target == signed) {
        return Err(invalid(
          "stable command path already belongs to another installation",
        ));
      }
    }
  }
  for name in crate::bundle_layout::ALL {
    if name == "ctld" {
      prepare_proxy(&root)?;
    } else {
      replace_link(&root.join(name), &Path::new("../current").join(name))?;
    }
  }
  Ok(())
}

#[cfg(unix)]
pub(super) fn prepare_activation(store: &Installations) -> io::Result<()> {
  prepare_aliases(store)
}

#[cfg(unix)]
pub(super) fn activate(
  store: &Installations,
  active: &Bundle,
  previous: Option<&Bundle>,
) -> io::Result<()> {
  validate_native(active)?;
  if let Some(previous) = previous {
    validate_native(previous)?;
  }
  prepare_aliases(store)?;
  let previous =
    previous.filter(|previous| previous.manifest.bundle_id != active.manifest.bundle_id);
  let current = store.root.join("current");
  if path_exists(&current)?
    && fs::read_link(&current)? == Path::new("installs").join(&active.manifest.bundle_id)
  {
    // Re-selecting the current payload also retires legacy rollback state.
    if previous.is_none() {
      clear_previous(store)?;
    }
    return Ok(());
  }
  let transaction = Transaction {
    active: active.manifest.bundle_id.clone(),
    previous: previous.map(|bundle| bundle.manifest.bundle_id.clone()),
  };
  let next = Temporary(
    store
      .root
      .join(format!(".transaction-{}", uuid::Uuid::new_v4())),
  );
  write(
    &next.0,
    &serde_json::to_vec(&transaction).map_err(io::Error::other)?,
    false,
  )?;
  if path_exists(&store.root.join(JOURNAL))? {
    return Err(invalid("installation transaction was not recovered"));
  }
  fs::rename(&next.0, store.root.join(JOURNAL))?;
  File::open(&store.root)?.sync_all()?;
  recover(store)
}

#[cfg(unix)]
pub(super) fn recover(store: &Installations) -> io::Result<()> {
  let path = store.root.join(JOURNAL);
  if !path_exists(&path)? {
    return Ok(());
  }
  let transaction: Transaction = serde_json::from_slice(&read(&path)?).map_err(io::Error::other)?;
  let active = load(store, &transaction.active)?;
  validate_native(&active)?;
  let previous = transaction
    .previous
    .as_deref()
    .map(|id| load(store, id))
    .transpose()?;
  if let Some(previous) = &previous {
    validate_native(previous)?;
    if previous.manifest.bundle_id == active.manifest.bundle_id {
      return Err(invalid("activation and rollback identities must differ"));
    }
  }
  let catalog = super::catalog::read_index(store)?;
  for bundle in [Some(&active), previous.as_ref()].into_iter().flatten() {
    if !catalog.entries.iter().any(|entry| {
      entry.bundle_id == bundle.manifest.bundle_id
        && entry.target_triple == bundle.manifest.target_triple
    }) {
      return Err(invalid(
        "activation journal selects an unrecorded installation",
      ));
    }
  }
  prepare_aliases(store)?;
  if let Some(previous) = previous {
    replace_link(
      &store.root.join("previous"),
      &Path::new("installs").join(previous.manifest.bundle_id),
    )?;
  } else {
    clear_previous(store)?;
  }
  replace_link(
    &store.root.join("current"),
    &Path::new("installs").join(active.manifest.bundle_id),
  )?;
  fs::remove_file(path)?;
  File::open(&store.root)?.sync_all()
}

#[cfg(unix)]
pub(super) fn clear_previous(store: &Installations) -> io::Result<()> {
  let path = store.root.join("previous");
  if path_exists(&path)? {
    // Accept only our canonical relative link so cleanup cannot remove an
    // unrelated file at this location.
    pointer_path(store, "previous")?;
    fs::remove_file(path)?;
    sync_directory(&store.root)?;
  }
  Ok(())
}

pub(super) fn entries(store: &Installations) -> io::Result<Vec<(String, PathBuf)>> {
  let root = store.root.join("installs");
  if !path_exists(&root)? {
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

pub(super) fn clean_staging(store: &Installations) -> io::Result<()> {
  let root = store.root.join("installs");
  if !path_exists(&root)? {
    return Ok(());
  }
  directory(&root)?;
  for entry in fs::read_dir(&root)? {
    let entry = entry?;
    let name = entry.file_name();
    let abandoned = name
      .to_str()
      .and_then(|name| name.strip_prefix(".partial-"))
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
      .open(store.root.join("installations.lock"))?;
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
