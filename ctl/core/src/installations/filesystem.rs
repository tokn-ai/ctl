use super::{Installations, catalog, leases};
use crate::bundles::{Bundle, MANIFEST_FILE, local_target};
use crate::component::ComponentBuildInfo;
use crate::private_fs::{self, options, sync_directory};
use std::collections::{BTreeMap, BTreeSet};
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
        "recorded runtime bundle {} has an incomplete payload under ctlup/store/runtime",
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
      "recorded runtime bundle {} is missing under ctlup/store/runtime",
      &id[..12]
    ),
  )
}

pub(super) fn load(store: &Installations, state: &catalog::State, id: &str) -> io::Result<Bundle> {
  load_available(store, state, id)?.ok_or_else(|| missing_payload(id))
}

pub(super) fn directory_if_present(path: &Path) -> io::Result<bool> {
  match directory(path) {
    Ok(()) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

pub(super) fn load_available(
  store: &Installations,
  state: &catalog::State,
  id: &str,
) -> io::Result<Option<Bundle>> {
  let Some(path) = payload_dir(store, state, id)? else {
    return Ok(None);
  };
  open_payload(&path, id).map(Some)
}

pub(super) fn payload_dir(
  store: &Installations,
  state: &catalog::State,
  id: &str,
) -> io::Result<Option<PathBuf>> {
  mapped_payload_dir(
    store,
    &store.runtime_root(),
    id,
    state.runtime_paths.location(id),
  )
}

fn mapped_payload_dir(
  store: &Installations,
  root: &Path,
  id: &str,
  location: Option<&str>,
) -> io::Result<Option<PathBuf>> {
  if !valid_id(id) {
    return Err(invalid("invalid installation identity"));
  }
  if !directory_if_present(&store.updater_root().join("store"))? || !directory_if_present(root)? {
    return Ok(None);
  }
  let location = location.unwrap_or(id);
  if let Some((group, _)) = location.split_once('/')
    && !directory_if_present(&root.join(group))?
  {
    return Ok(None);
  }
  let path = root.join(location);
  Ok(directory_if_present(&path)?.then_some(path))
}

fn open_payload(path: &Path, id: &str) -> io::Result<Bundle> {
  let bundle = Bundle::open(path).map_err(|error| incomplete_payload(error, id))?;
  validate_complete(&bundle)?;
  if bundle.manifest.bundle_id != id {
    return Err(invalid(
      "installation payload identity differs from its registry entry",
    ));
  }
  Ok(bundle)
}

pub(super) fn publish(
  store: &Installations,
  state: &mut catalog::State,
  source: &Bundle,
  files: Option<&BTreeMap<String, Vec<u8>>>,
  origin: Option<&catalog::CatalogOrigin>,
) -> io::Result<Bundle> {
  validate_complete(source)?;
  let id = &source.manifest.bundle_id;
  if let Some(installed) = load_available(store, state, id)? {
    if installed.manifest != source.manifest {
      return Err(invalid(
        "immutable installation already contains different metadata",
      ));
    }
    return Ok(installed);
  }
  if state.updater.is_some()
    && state.updater_paths.object.launcher_layout != Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION)
  {
    return Err(invalid(
      "provision the current ctlup before adding runtime builds to this profile",
    ));
  }
  let target = &source.manifest.target_triple;
  let root = store.runtime_root();
  let known_location = state.runtime_paths.location(id);
  let location = if let Some(location) = known_location {
    location.to_owned()
  } else {
    let label = crate::installed_build_name::label(
      &source.manifest.components["ctl-agent"].build,
      origin,
      id,
    );
    available_location(&label, id, Some(target), &root, |location| {
      state.runtime_paths.reserves(location)
    })?
  };
  let destination = root.join(&location);
  let owned;
  let files = if let Some(files) = files {
    files
  } else {
    owned = source.read_files()?;
    &owned
  };
  source.manifest.verify_files(files)?;
  let workspace = Temporary(
    store
      .staging_root()
      .join(format!("runtime-{}", uuid::Uuid::new_v4())),
  );
  ensure_directory(&workspace.0)?;
  for (name, bytes) in files {
    let path = workspace.0.join(name);
    let mut parent = workspace.0.clone();
    for part in path.parent().unwrap().strip_prefix(&workspace.0).unwrap() {
      parent.push(part);
      ensure_directory(&parent)?;
    }
    write(&path, bytes, source.manifest.files[name].executable)?;
  }
  write(
    &workspace.0.join(MANIFEST_FILE),
    &serde_json::to_vec(&source.manifest).map_err(io::Error::other)?,
    false,
  )?;
  Bundle::open(&workspace.0)?;
  sync_tree(&workspace.0)?;
  if known_location.is_none() {
    state
      .runtime_paths
      .build_first
      .locations
      .insert(id.clone(), location);
    // The path reservation is durable before the rename. An interrupted
    // publisher can resume, and pruning can find a completed orphan.
    catalog::commit_runtime_build_paths(store, &state.runtime_paths.build_first)?;
  }
  let group = destination.parent().unwrap();
  ensure_directory(group)?;
  if path_exists(&destination)? {
    return Err(invalid("runtime payload location already exists"));
  }
  fs::rename(&workspace.0, &destination)?;
  sync_directory(group)?;
  sync_directory(&root)?;
  sync_directory(&store.staging_root())?;
  open_payload(&destination, id)
}

fn available_location(
  label: &str,
  id: &str,
  target: Option<&str>,
  root: &Path,
  reserves: impl Fn(&str) -> bool,
) -> io::Result<String> {
  for name in [
    label,
    &format!("{label}-{}", &id[..12]),
    &format!("{label}-{id}"),
  ] {
    if catalog::validate_payload_label(name).is_err() {
      continue;
    }
    let location = match target {
      Some(target) => format!("{name}/{target}"),
      None => name.to_owned(),
    };
    if reserves(&location) {
      continue;
    }
    let group = root.join(name);
    let occupied = match target {
      Some(target) => directory_if_present(&group)? && path_exists(&group.join(target))?,
      None => path_exists(&group)?,
    };
    if !occupied {
      return Ok(location);
    }
  }
  Err(invalid(
    "no unused installed payload location for this build",
  ))
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
fn prepare_aliases(store: &Installations, updater: Option<&[u8]>, refresh: bool) -> io::Result<()> {
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
  if let Some(bytes) = updater.filter(|_| !has_launcher || refresh) {
    replace_launcher(&launcher, bytes)?;
  }
  for alias in aliases {
    symlink(LAUNCHER, alias)?;
  }
  sync_directory(&bin)
}

pub(super) fn prepare_activation(store: &Installations) -> io::Result<()> {
  prepare_aliases(store, None, false)
}

pub(super) fn install_manager(
  store: &Installations,
  updater: &[u8],
  build: &ComponentBuildInfo,
  origin: Option<&catalog::CatalogOrigin>,
) -> io::Result<()> {
  if updater.is_empty() || updater.len() > crate::bundles::MAX_FILE_BYTES {
    return Err(invalid("invalid updater executable size"));
  }
  if let Some(origin) = origin {
    origin.validate()?;
  }
  ensure_root(store)?;
  let _lock = Lock::acquire(store)?;
  let mut state = catalog::read_index(store)?;
  catalog::initialize(store, &state)?;
  let root = store.manager_root();
  ensure_directory(&root)?;
  let id = crate::bundles::digest(updater);
  if read_manager(store, &state.updater_paths, &id)?.is_none() {
    let known_location = state.updater_paths.location(&id);
    let location = if let Some(location) = known_location {
      location.to_owned()
    } else {
      let label = crate::installed_build_name::label(build, origin, &id);
      available_location(&label, &id, None, &root, |location| {
        state.updater_paths.reserves(location)
      })?
    };
    let destination = root.join(&location);
    let destination_parent = destination.parent().unwrap();
    let temporary = Temporary(
      store
        .staging_root()
        .join(format!("updater-{}", uuid::Uuid::new_v4())),
    );
    ensure_directory(&temporary.0)?;
    write(&temporary.0.join(MANAGER), updater, true)?;
    sync_directory(&temporary.0)?;
    if known_location.is_none() {
      state
        .updater_paths
        .object
        .locations
        .insert(id.clone(), location);
      catalog::commit_updater_object_paths(store, &state.updater_paths.object)?;
    }
    ensure_directory(destination_parent)?;
    if path_exists(&destination)? {
      return Err(invalid("updater payload location already exists"));
    }
    fs::rename(&temporary.0, &destination)?;
    sync_directory(destination_parent)?;
    sync_directory(&root)?;
    sync_directory(&store.staging_root())?;
  }
  let refresh =
    state.updater_paths.object.launcher_layout != Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION);
  prepare_aliases(store, Some(updater), refresh)?;
  if refresh {
    state.updater_paths.object.launcher_layout = Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION);
    catalog::commit_updater_object_paths(store, &state.updater_paths.object)?;
  }
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
  let state = catalog::read_index(store)?;
  let Some(id) = state.updater.as_deref() else {
    return Ok(None);
  };
  let (path, _) = read_manager(store, &state.updater_paths, id)?.ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "selected updater payload is missing",
    )
  })?;
  let lease = leases::retain(&store.manager_leases(), id)?;
  Ok(Some((path, lease)))
}

pub(super) fn read_manager(
  store: &Installations,
  paths: &catalog::PayloadPaths,
  id: &str,
) -> io::Result<Option<(PathBuf, Vec<u8>)>> {
  let Some(payload) = mapped_payload_dir(store, &store.manager_root(), id, paths.location(id))?
  else {
    return Ok(None);
  };
  let path = payload.join(MANAGER);
  let bytes = executable_bytes(&path).map_err(|error| {
    if error.kind() == io::ErrorKind::NotFound {
      invalid("stored updater has an incomplete payload")
    } else {
      error
    }
  })?;
  if crate::bundles::digest(&bytes) != id {
    return Err(invalid("stored updater failed payload verification"));
  }
  Ok(Some((path, bytes)))
}

pub(super) fn prune_managers(store: &Installations, state: &mut catalog::State) -> io::Result<()> {
  let active = state.updater.as_deref();
  let leases_root = store.manager_leases();
  let transitioned =
    state.updater_paths.object.launcher_layout == Some(super::UPDATER_OBJECT_PATH_LAYOUT_VERSION);
  let mut retained = BTreeSet::new();
  for (id, path) in mapped_payloads(
    store,
    &store.manager_root(),
    state.updater_paths.locations(),
  )? {
    // A failed Windows refresh may have already copied some aliases from an
    // installed candidate. Retain this candidate until the transition resumes.
    if active == Some(id.as_str())
      || (!transitioned && state.updater_paths.location(&id).is_some())
      || leases::in_use_at(&leases_root, &id)?
    {
      retained.insert(id);
    } else {
      remove_installation(&path)?;
    }
  }
  let root = store.manager_root();
  let keep = |id: &str| active == Some(id) || retained.contains(id);
  if release_unused_locations(&root, &mut state.updater_paths.target_first, keep)? {
    catalog::commit_updater_paths(store, &state.updater_paths.target_first)?;
  }
  if release_unused_locations(&root, &mut state.updater_paths.build_first, keep)? {
    catalog::commit_updater_build_paths(store, &state.updater_paths.build_first)?;
  }
  if release_unused_locations(&root, &mut state.updater_paths.object, keep)? {
    catalog::commit_updater_object_paths(store, &state.updater_paths.object)?;
  }
  Ok(())
}

pub(super) fn desktop_object_path(
  store: &Installations,
  paths: &catalog::PathIndex,
  id: &str,
) -> io::Result<PathBuf> {
  let root = store.desktop_root();
  let location = paths.location(id);
  let existing = mapped_payload_dir(store, &root, id, location)?;
  Ok(existing.unwrap_or_else(|| root.join(location.unwrap_or(id))))
}

pub(super) fn reserve_desktop_object(
  store: &Installations,
  state: &mut catalog::State,
  id: &str,
  label: Option<&str>,
) -> io::Result<PathBuf> {
  let path = desktop_object_path(store, &state.desktop_paths, id)?;
  let Some(label) = label else {
    return Ok(path);
  };
  if state.desktop_paths.location(id).is_some()
    || state
      .desktop
      .as_ref()
      .is_some_and(|active| active.content_id == id)
    || path_exists(&path)?
  {
    return Ok(path);
  }
  let root = store.desktop_root();
  let location = available_location(label, id, None, &root, |candidate| {
    state
      .desktop_paths
      .locations
      .values()
      .any(|location| location.eq_ignore_ascii_case(candidate))
  })?;
  let path = root.join(&location);
  state
    .desktop_paths
    .locations
    .insert(id.to_owned(), location);
  // Keep a durable path reservation before a callback can publish bytes.
  catalog::commit_desktop_paths(store, &state.desktop_paths)?;
  Ok(path)
}

pub(super) fn desktop_available(
  store: &Installations,
  paths: &catalog::PathIndex,
  id: &str,
) -> io::Result<bool> {
  Ok(mapped_payload_dir(store, &store.desktop_root(), id, paths.location(id))?.is_some())
}

pub(super) fn prune_desktops(store: &Installations, state: &catalog::State) -> io::Result<()> {
  let active = state
    .desktop
    .as_ref()
    .map(|selection| selection.content_id.as_str());
  let leases_root = store.desktop_leases();
  for (id, path) in mapped_payloads(
    store,
    &store.desktop_root(),
    state.desktop_paths.locations.iter(),
  )? {
    if active != Some(id.as_str()) && !leases::in_use_at(&leases_root, &id)? {
      remove_installation(&path)?;
    }
  }
  Ok(())
}

pub(super) fn release_desktop_paths(
  store: &Installations,
  state: &mut catalog::State,
) -> io::Result<()> {
  let active = state
    .desktop
    .as_ref()
    .map(|selection| selection.content_id.as_str());
  let leases_root = store.desktop_leases();
  let mut unused = BTreeSet::new();
  for id in state.desktop_paths.locations.keys() {
    if active != Some(id.as_str())
      && !desktop_available(store, &state.desktop_paths, id)?
      && !leases::in_use_at(&leases_root, id)?
    {
      unused.insert(id.clone());
    }
  }
  if !unused.is_empty() {
    state
      .desktop_paths
      .locations
      .retain(|id, _| !unused.contains(id));
    catalog::commit_desktop_paths(store, &state.desktop_paths)?;
  }
  Ok(())
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

pub(super) fn entries(
  store: &Installations,
  state: &catalog::State,
) -> io::Result<Vec<(String, PathBuf)>> {
  mapped_payloads(
    store,
    &store.runtime_root(),
    state.runtime_paths.locations(),
  )
}

fn mapped_payloads<'a>(
  store: &Installations,
  root: &Path,
  locations: impl Iterator<Item = (&'a String, &'a String)>,
) -> io::Result<Vec<(String, PathBuf)>> {
  if !directory_if_present(&store.updater_root().join("store"))? || !directory_if_present(root)? {
    return Ok(Vec::new());
  }
  let locations: BTreeMap<_, _> = locations
    .map(|(id, location)| (id.as_str(), location.as_str()))
    .collect();
  let mut entries = Vec::new();
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let id = entry
      .file_name()
      .to_str()
      .map(str::to_owned)
      .unwrap_or_default();
    if valid_id(&id) && !locations.contains_key(id.as_str()) {
      directory(&entry.path())?;
      entries.push((id, entry.path()));
    }
  }
  for (id, location) in locations {
    if let Some(path) = mapped_payload_dir(store, root, id, Some(location))? {
      entries.push((id.to_owned(), path));
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

/// Retire unclaimed path records only after the caller has removed their
/// payloads. A known group shared with another target remains on disk.
pub(super) fn release_unused_locations(
  root: &Path,
  paths: &mut catalog::PathIndex,
  keep: impl Fn(&str) -> bool,
) -> io::Result<bool> {
  let unused: BTreeSet<_> = paths
    .locations
    .keys()
    .filter(|id| !keep(id))
    .cloned()
    .collect();
  if unused.is_empty() {
    return Ok(false);
  }
  if directory_if_present(root)? {
    let mut cleaned = false;
    for location in unused.iter().map(|id| &paths.locations[id]) {
      let Some((name, _)) = location.split_once('/') else {
        continue;
      };
      let group = root.join(name);
      if directory_if_present(&group)? {
        match fs::remove_dir(group) {
          Ok(()) => {
            cleaned = true;
          }
          Err(error)
            if matches!(
              error.kind(),
              io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) => {}
          Err(error) => return Err(error),
        }
      }
    }
    if cleaned {
      sync_directory(root)?;
    }
  }
  paths.locations.retain(|id, _| !unused.contains(id));
  Ok(true)
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
