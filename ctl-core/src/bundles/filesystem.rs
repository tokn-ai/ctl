use super::{
  Bundle, MANIFEST_FILE, MAX_FILE_BYTES, MAX_MANIFEST_BYTES, Manifest, Purpose, Store, invalid,
  valid_digest, validate_target,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{
  DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  bundle_id: String,
  target_triple: String,
}

pub(super) fn selected(
  store: &Store,
  purpose: Purpose,
  target: &str,
) -> io::Result<Option<Bundle>> {
  validate_target(target)?;
  if purpose == Purpose::Local && !super::local_target(target) {
    return Ok(None);
  }
  if !root_exists(store)? {
    return Ok(None);
  }
  let selected = store.root.join("selected");
  if !directory_exists(&selected)? {
    return Ok(None);
  }
  let path = selection_path(store, purpose, target);
  let bytes = match read(&path, 1024) {
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    result => result?,
  };
  let selection: Selection = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  if !valid_digest(&selection.bundle_id) {
    return Err(invalid("invalid bundle selection"));
  }
  validate_target(&selection.target_triple)?;
  if purpose == Purpose::Upload && selection.target_triple != target {
    return Err(invalid("upload selection target differs from its profile"));
  }
  if purpose == Purpose::Local {
    if !super::local_target(&selection.target_triple) {
      return Err(invalid("local selection has an incompatible target"));
    }
    if target != crate::paths::native_target() && target != selection.target_triple {
      return Ok(None);
    }
  }
  load(store, &selection.target_triple, &selection.bundle_id).map(Some)
}

pub(super) fn list(store: &Store, target: &str) -> io::Result<Vec<Bundle>> {
  validate_target(target)?;
  if !root_exists(store)? {
    return Ok(Vec::new());
  }
  let root = store.root.join("bundles");
  if !directory_exists(&root)? {
    return Ok(Vec::new());
  }
  let root = root.join(target);
  if !directory_exists(&root)? {
    return Ok(Vec::new());
  }
  let mut ids = Vec::new();
  for entry in fs::read_dir(&root)? {
    let name = entry?.file_name();
    if let Some(id) = name.to_str().filter(|id| valid_digest(id)) {
      ids.push(id.to_owned());
      if ids.len() > 4096 {
        return Err(invalid("too many stored component bundles"));
      }
    }
  }
  ids.sort();
  ids.into_iter().map(|id| load(store, target, &id)).collect()
}

pub(super) fn load(store: &Store, target: &str, id: &str) -> io::Result<Bundle> {
  if !root_exists(store)? {
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      "component store is absent",
    ));
  }
  let root = store.root.join("bundles");
  require_directory(&root)?;
  require_directory(&root.join(target))?;
  let directory = root.join(target).join(id);
  let manifest = read_manifest(&directory)?;
  if manifest.bundle_id != id || manifest.target_triple != target {
    return Err(invalid("bundle identity differs from its immutable path"));
  }
  verify_payload(&directory, &manifest)?;
  Ok(Bundle {
    directory,
    manifest,
  })
}

/// Used by the agent for an executable-pinned directory, without selection lookup.
pub(crate) fn read_manifest(directory: &Path) -> io::Result<Manifest> {
  require_directory(directory)?;
  let bytes = read(&directory.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
  let manifest: Manifest = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  manifest.validate()?;
  Ok(manifest)
}

pub(super) fn verify_payload(directory: &Path, manifest: &Manifest) -> io::Result<()> {
  verify_tree(directory, directory, manifest)?;
  let mut total = 0usize;
  for (name, info) in &manifest.files {
    let path = directory.join(name);
    check_parents(directory, &path)?;
    let bytes = read(&path, MAX_FILE_BYTES)?;
    total = total.saturating_add(bytes.len());
    if bytes.is_empty()
      || total > super::MAX_TOTAL_BYTES
      || super::digest(&bytes) != info.sha256
      || (info.executable && fs::symlink_metadata(&path)?.mode() & 0o111 == 0)
    {
      return Err(invalid(
        "stored component bundle failed payload verification",
      ));
    }
  }
  Ok(())
}

fn verify_tree(root: &Path, directory: &Path, manifest: &Manifest) -> io::Result<()> {
  require_directory(directory)?;
  for entry in fs::read_dir(directory)? {
    let entry = entry?;
    let path = entry.path();
    let name = path
      .strip_prefix(root)
      .map_err(io::Error::other)?
      .to_str()
      .ok_or_else(|| invalid("invalid bundle payload path"))?;
    if name.len() > 512 {
      return Err(invalid("excessive bundle payload path"));
    }
    if entry.file_type()?.is_dir()
      && manifest
        .files
        .keys()
        .any(|file| file.starts_with(&format!("{name}/")))
    {
      verify_tree(root, &path, manifest)?;
    } else if entry.file_type()?.is_file()
      && (name == MANIFEST_FILE || manifest.files.contains_key(name))
    {
      require_file(&entry.metadata()?)?;
    } else {
      return Err(invalid("unexpected file in immutable component bundle"));
    }
  }
  Ok(())
}

pub(super) fn payload(bundle: &Bundle) -> io::Result<BTreeMap<String, Vec<u8>>> {
  bundle.manifest.validate()?;
  if read_manifest(&bundle.directory)? != bundle.manifest {
    return Err(invalid("component bundle metadata changed"));
  }
  verify_tree(&bundle.directory, &bundle.directory, &bundle.manifest)?;
  let mut files = BTreeMap::new();
  let mut total = 0usize;
  for (name, info) in &bundle.manifest.files {
    let path = bundle.directory.join(name);
    check_parents(&bundle.directory, &path)?;
    let bytes = read(&path, MAX_FILE_BYTES)?;
    if info.executable && fs::symlink_metadata(&path)?.mode() & 0o111 == 0 {
      return Err(invalid("component bundle executable permissions changed"));
    }
    total = total.saturating_add(bytes.len());
    if total > super::MAX_TOTAL_BYTES {
      return Err(invalid("component bundle exceeds its total size limit"));
    }
    files.insert(name.clone(), bytes);
  }
  bundle.manifest.verify_files(&files)?;
  Ok(files)
}

pub(super) fn publish(
  store: &Store,
  manifest: &Manifest,
  files: &BTreeMap<String, Vec<u8>>,
) -> io::Result<Bundle> {
  manifest.validate()?;
  manifest.verify_files(files)?;
  ensure_root(store)?;
  let _lock = Lock::acquire(&store.root.join("bundle.lock"))?;
  let root = store.root.join("bundles");
  ensure_directory(&root)?;
  let root = root.join(&manifest.target_triple);
  ensure_directory(&root)?;
  let destination = root.join(&manifest.bundle_id);
  if directory_exists(&destination)? {
    let existing = load(store, &manifest.target_triple, &manifest.bundle_id)?;
    if existing.manifest != *manifest {
      return Err(invalid(
        "immutable bundle already contains different metadata",
      ));
    }
    return Ok(existing);
  }
  let stage = Temporary(root.join(format!(".partial-{}", uuid::Uuid::new_v4())));
  ensure_directory(&stage.0)?;
  for (name, bytes) in files {
    let path = stage.0.join(name);
    ensure_descendants(&stage.0, path.parent().unwrap())?;
    write(&path, bytes, manifest.files[name].executable)?;
  }
  write(
    &stage.0.join(MANIFEST_FILE),
    &serde_json::to_vec(manifest).map_err(io::Error::other)?,
    false,
  )?;
  sync_tree(&stage.0)?;
  fs::rename(&stage.0, &destination)?;
  File::open(&root)?.sync_all()?;
  load(store, &manifest.target_triple, &manifest.bundle_id)
}

pub(super) fn select(
  store: &Store,
  purpose: Purpose,
  bundle: &Bundle,
  only_unset: bool,
) -> io::Result<()> {
  ensure_root(store)?;
  let _lock = Lock::acquire(&store.root.join("bundle.lock"))?;
  if only_unset && selected(store, purpose, &bundle.manifest.target_triple)?.is_some() {
    return Ok(());
  }
  let current = load(
    store,
    &bundle.manifest.target_triple,
    &bundle.manifest.bundle_id,
  )?;
  if current.manifest != bundle.manifest || current.directory != bundle.directory {
    return Err(invalid("selected bundle changed"));
  }
  if purpose == Purpose::Local {
    if !super::local_target(&bundle.manifest.target_triple) {
      return Err(invalid("a local bundle must run on this computer"));
    }
    if cfg!(target_os = "macos")
      && (!bundle
        .manifest
        .files
        .contains_key("ctld.app/Contents/MacOS/ctld")
        || !bundle.manifest.files.contains_key("ctld-package.json"))
    {
      return Err(invalid(
        "local macOS bundles require the complete provisioned ctld.app and package receipt",
      ));
    }
  }
  let directory = store.root.join("selected");
  ensure_directory(&directory)?;
  let path = selection_path(store, purpose, &bundle.manifest.target_triple);
  match fs::symlink_metadata(&path) {
    Ok(metadata) => require_file(&metadata)?,
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  let next = Temporary(directory.join(format!(".next-{}", uuid::Uuid::new_v4())));
  write(
    &next.0,
    &serde_json::to_vec(&Selection {
      bundle_id: bundle.manifest.bundle_id.clone(),
      target_triple: bundle.manifest.target_triple.clone(),
    })
    .map_err(io::Error::other)?,
    false,
  )?;
  fs::rename(&next.0, path)?;
  File::open(&directory)?.sync_all()
}

fn selection_path(store: &Store, purpose: Purpose, target: &str) -> PathBuf {
  let target = if purpose == Purpose::Local {
    match crate::paths::native_target() {
      "x86_64-unknown-linux-gnu" => "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-gnu" => "aarch64-unknown-linux-musl",
      native => native,
    }
  } else {
    target
  };
  store
    .root
    .join("selected")
    .join(format!("{}-{target}.json", purpose.name()))
}

fn ancestors(store: &Store) -> [PathBuf; 4] {
  let ctl = store.root.parent().unwrap();
  let tokn = ctl.parent().unwrap();
  [
    tokn.parent().unwrap().to_owned(),
    tokn.to_owned(),
    ctl.to_owned(),
    store.root.clone(),
  ]
}

fn root_exists(store: &Store) -> io::Result<bool> {
  for path in ancestors(store) {
    if !directory_exists(&path)? {
      return Ok(false);
    }
  }
  Ok(true)
}

fn ensure_root(store: &Store) -> io::Result<()> {
  for path in ancestors(store) {
    ensure_directory(&path)?;
  }
  Ok(())
}

fn directory_exists(path: &Path) -> io::Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => {
      require_directory_metadata(&metadata)?;
      Ok(true)
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn require_directory(path: &Path) -> io::Result<()> {
  require_directory_metadata(&fs::symlink_metadata(path)?)
}
fn require_directory_metadata(metadata: &Metadata) -> io::Result<()> {
  if !metadata.is_dir()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o022 != 0
  {
    return Err(unsafe_path());
  }
  Ok(())
}
fn require_file(metadata: &Metadata) -> io::Result<()> {
  if !metadata.is_file()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(unsafe_path());
  }
  Ok(())
}
fn ensure_directory(path: &Path) -> io::Result<()> {
  if directory_exists(path)? {
    return Ok(());
  }
  match fs::DirBuilder::new().mode(0o700).create(path) {
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => require_directory(path),
    result => result,
  }
}
fn ensure_descendants(root: &Path, path: &Path) -> io::Result<()> {
  let relative = path.strip_prefix(root).map_err(io::Error::other)?;
  let mut parent = root.to_owned();
  for part in relative {
    parent.push(part);
    ensure_directory(&parent)?;
  }
  Ok(())
}
fn check_parents(root: &Path, path: &Path) -> io::Result<()> {
  let relative = path
    .parent()
    .unwrap()
    .strip_prefix(root)
    .map_err(io::Error::other)?;
  let mut parent = root.to_owned();
  for part in relative {
    parent.push(part);
    require_directory(&parent)?;
  }
  Ok(())
}
fn options() -> OpenOptions {
  let mut options = OpenOptions::new();
  options.mode(0o600).custom_flags(i32::from_ne_bytes(
    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC)
      .bits()
      .to_ne_bytes(),
  ));
  options
}
fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
  require_file(&fs::symlink_metadata(path)?)?;
  let file = options().read(true).open(path)?;
  require_file(&file.metadata()?)?;
  let mut bytes = Vec::new();
  file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > maximum {
    return Err(invalid("component bundle file exceeds its size limit"));
  }
  Ok(bytes)
}
fn write(path: &Path, bytes: &[u8], executable: bool) -> io::Result<()> {
  let mut file = options().write(true).create_new(true).open(path)?;
  file.write_all(bytes)?;
  if executable {
    file.set_permissions(fs::Permissions::from_mode(0o700))?;
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
  File::open(path)?.sync_all()
}
struct Lock(File);
impl Lock {
  fn acquire(path: &Path) -> io::Result<Self> {
    match fs::symlink_metadata(path) {
      Ok(metadata) => require_file(&metadata)?,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    let file = options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(path)?;
    require_file(&file.metadata()?)?;
    file.try_lock().map_err(|error| match error {
      fs::TryLockError::WouldBlock => io::Error::new(
        io::ErrorKind::WouldBlock,
        "another component sync is running",
      ),
      fs::TryLockError::Error(error) => error,
    })?;
    Ok(Self(file))
  }
}
impl Drop for Lock {
  fn drop(&mut self) {
    let _ = self.0.unlock();
  }
}
struct Temporary(PathBuf);
impl Drop for Temporary {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
    let _ = fs::remove_dir_all(&self.0);
  }
}
fn unsafe_path() -> io::Error {
  io::Error::new(
    io::ErrorKind::PermissionDenied,
    "component store paths must be owned directories or private regular files, not links or special files",
  )
}
