use super::{CachedPackage, DesktopMetadata, DesktopPayload, Digest, app_destination, invalid};
use ctl_core::installations::{DesktopSelection, Installations};
use ctl_core::private_fs;
use std::{
  fs::{self, File, Metadata},
  io::{self, Write as _},
  path::{Path, PathBuf},
};

pub(super) struct Workspace {
  installations: Installations,
  store: PathBuf,
  downloads: PathBuf,
  staging: PathBuf,
  destination: PathBuf,
  _lock: File,
}

impl Workspace {
  pub(super) fn begin(home: &Path) -> io::Result<Self> {
    // Validate the public profile and application directory before creating
    // anything in the adjacent ctlup store.
    ctl_client::desktop_storage::app_directory(home, true)?;
    let installations = Installations::new(home);
    let root = installations.updater_root();
    ensure_directory(&root)?;
    let state = root.join("state");
    ensure_directory(&state)?;
    let lock = private_fs::options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(state.join("desktop.lock"))?;
    if !private_fs::is_private_file(&lock.metadata()?) {
      return Err(invalid("desktop install lock must be a private owned file"));
    }
    lock.try_lock().map_err(|error| match error {
      fs::TryLockError::WouldBlock => io::Error::new(
        io::ErrorKind::WouldBlock,
        "another ctmux desktop installation is in progress",
      ),
      fs::TryLockError::Error(error) => error,
    })?;
    let store = ensure_subdirectories(&root, ["store", "desktop"])?;
    let downloads = ensure_subdirectories(&root, ["cache", "downloads", "desktop"])?;
    let staging_root = ensure_subdirectories(&root, ["staging"])?;
    clean_abandoned_staging(&staging_root)?;
    let staging = staging_root.join(format!("desktop-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&staging)?;
    let destination = app_destination(home)?;
    Ok(Self {
      installations,
      store,
      downloads,
      staging,
      destination,
      _lock: lock,
    })
  }

  pub(super) fn installations(&self) -> &Installations {
    &self.installations
  }

  pub(super) fn staging(&self) -> &Path {
    &self.staging
  }

  pub(super) fn destination(&self) -> &Path {
    &self.destination
  }

  pub(super) fn existing(&self, digest: &Digest) -> io::Result<Option<PathBuf>> {
    let object = self.store.join(digest.as_str());
    if let Some(metadata) = metadata(&object)? {
      if !private_fs::is_directory(&metadata) {
        return Err(invalid("unsafe desktop payload directory"));
      }
      let payload = object.join(super::payload_name());
      if metadata_for_payload(&payload)?.is_some() {
        return Ok(Some(payload));
      }
    }
    Ok(None)
  }

  pub(super) fn cached(&self, digest: &Digest) -> io::Result<Option<CachedPackage>> {
    let directory = self.downloads.join(digest.as_str());
    cache_package(&directory, digest)
  }

  pub(super) fn cache(&self, package: &CachedPackage) -> io::Result<()> {
    let directory = self.downloads.join(package.metadata.content_id.as_str());
    let cached = self.cached(&package.metadata.content_id)?;
    if let Some(existing) = cached.as_ref() {
      match super::package_digest(&existing.archive) {
        Ok(actual) if actual == package.metadata.content_id => {
          return self.refresh_metadata(&directory, &package.metadata, &existing.metadata);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
      }
    }
    let temporary = self.staging.join("download");
    private_fs::create_directory(&temporary)?;
    let archive = temporary.join("package");
    super::copy_package(&package.archive, &archive)?;
    write_metadata(&temporary, &package.metadata)?;
    private_fs::sync_directory(&temporary)?;
    if cached.is_some() {
      // Replace a damaged cache entry only once a verified copy is staged.
      fs::rename(&directory, self.staging.join("discarded-cache-entry"))?;
    }
    fs::rename(&temporary, &directory)?;
    private_fs::sync_directory(&self.downloads)
  }

  pub(super) fn activate(
    &self,
    prepared: &DesktopPayload,
    expected: Option<&DesktopSelection>,
    selected: &DesktopSelection,
  ) -> io::Result<bool> {
    let (launch, reused) = self
      .installations
      .publish_desktop(expected, selected, || self.publish_payload(prepared))?;
    launch.publish()?;
    Ok(reused)
  }

  fn publish_payload(&self, prepared: &DesktopPayload) -> io::Result<(LaunchEntry, bool)> {
    let object = self.store.join(prepared.metadata.content_id.as_str());
    let payload = object.join(super::payload_name());
    let previous = self.staging.join("replaced-payload");
    if self.existing(&prepared.metadata.content_id)?.is_some()
      && super::payload_matches(&payload, &prepared.path)?
    {
      let metadata = read_metadata(&object, &prepared.metadata.content_id)?;
      self.refresh_metadata(&object, &prepared.metadata, &metadata)?;
      return Ok((self.prepare_launch_entry(&payload)?, true));
    }
    // Prepare the complete object privately. A damaged object may be repaired
    // only after its owner has released it. If publication fails, restore the
    // previous directory and leave the installation selection untouched.
    let staged = self.staging.join("object");
    private_fs::create_directory(&staged)?;
    fs::rename(&prepared.path, staged.join(super::payload_name()))?;
    write_metadata(&staged, &prepared.metadata)?;
    private_fs::sync_directory(&staged)?;
    let replaced = if let Some(info) = metadata(&object)? {
      if !private_fs::is_directory(&info) {
        return Err(invalid("unsafe desktop payload directory"));
      }
      #[cfg(target_os = "macos")]
      if payload.is_dir() && super::macos::is_running(&payload)? {
        return Err(io::Error::other(
          "Quit the ctmux desktop app before repairing its current build.",
        ));
      }
      fs::rename(&object, &previous)?;
      true
    } else {
      false
    };
    if let Err(error) = fs::rename(&staged, &object) {
      if replaced {
        fs::rename(previous, object)?;
      }
      return Err(error);
    }
    private_fs::sync_directory(&self.store)?;
    Ok((self.prepare_launch_entry(&payload)?, false))
  }

  pub(super) fn refresh(&self, selected: &DesktopSelection) -> io::Result<()> {
    let digest = Digest::parse(&selected.content_id)?;
    if let Some(payload) = self.existing(&digest)? {
      self.prepare_launch_entry(&payload)?.publish()?;
    } else {
      self.clear_launch_entry()?;
    }
    Ok(())
  }

  pub(super) fn clear_launch_entry(&self) -> io::Result<()> {
    if self.managed_entry()? {
      fs::remove_file(self.destination())?;
    }
    Ok(())
  }

  pub(super) fn prune_downloads(&self, selected: Option<&DesktopSelection>) -> io::Result<()> {
    for entry in fs::read_dir(&self.downloads)? {
      let entry = entry?;
      let identity = entry.file_name();
      let Some(identity) = identity.to_str() else {
        continue;
      };
      if Digest::parse(identity).is_err()
        || selected.is_some_and(|selection| selection.content_id == identity)
      {
        continue;
      }
      let path = entry.path();
      if !private_fs::is_directory(&fs::symlink_metadata(&path)?) {
        return Err(invalid("unsafe cached desktop package directory"));
      }
      fs::remove_dir_all(path)?;
    }
    private_fs::sync_directory(&self.downloads)
  }

  fn managed_entry(&self) -> io::Result<bool> {
    let destination = self.destination();
    let Some(info) = metadata(destination)? else {
      return Ok(false);
    };
    #[cfg(unix)]
    let managed = info.file_type().is_symlink()
      && managed_relative_link(
        &fs::read_link(destination)?,
        &self.store,
        self.installations.root(),
      );
    #[cfg(windows)]
    let managed =
      private_fs::is_owned_file(&info) && super::shortcut::managed(destination, &self.store)?;
    if !managed {
      return Err(invalid(
        "desktop launch path is occupied by an unmanaged file or app",
      ));
    }
    Ok(true)
  }

  fn refresh_metadata(
    &self,
    directory: &Path,
    desired: &DesktopMetadata,
    current: &DesktopMetadata,
  ) -> io::Result<()> {
    if desired == current {
      return Ok(());
    }
    let temporary = self
      .staging
      .join(format!("receipt-{}", uuid::Uuid::new_v4()));
    write_metadata_file(&temporary, desired)?;
    fs::rename(temporary, directory.join("desktop.json"))?;
    private_fs::sync_directory(directory)
  }

  fn prepare_launch_entry(&self, payload: &Path) -> io::Result<LaunchEntry> {
    let destination = self.destination();
    self.managed_entry()?;
    let temp = self.staging.join(format!(
      ".ctmux-{}{}",
      uuid::Uuid::new_v4(),
      launch_extension()
    ));
    let entry = LaunchEntry {
      destination: destination.to_owned(),
      staged: temp,
    };
    #[cfg(unix)]
    {
      let relative = payload
        .strip_prefix(self.installations.root())
        .map_err(io::Error::other)?;
      std::os::unix::fs::symlink(Path::new("..").join(relative), &entry.staged)?;
    }
    #[cfg(windows)]
    {
      super::shortcut::create(&entry.staged, &payload.join("ctmux-app.exe"))?;
    }
    Ok(entry)
  }
}

#[cfg(unix)]
fn managed_relative_link(link: &Path, store: &Path, profile: &Path) -> bool {
  let Ok(relative_store) = store.strip_prefix(profile) else {
    return false;
  };
  let prefix = Path::new("..").join(relative_store);
  let Ok(tail) = link.strip_prefix(prefix) else {
    return false;
  };
  let Some((directory, name)) = tail.parent().zip(tail.file_name()) else {
    return false;
  };
  let Some(digest) = directory.to_str() else {
    return false;
  };
  Digest::parse(digest).is_ok() && name == super::payload_name()
}

struct LaunchEntry {
  destination: PathBuf,
  staged: PathBuf,
}

impl LaunchEntry {
  fn publish(self) -> io::Result<()> {
    fs::rename(&self.staged, &self.destination)?;
    if let Some(parent) = self.destination.parent() {
      private_fs::sync_directory(parent)?;
    }
    Ok(())
  }
}

impl Drop for LaunchEntry {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.staged);
  }
}

impl Drop for Workspace {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.staging);
  }
}

fn launch_extension() -> &'static str {
  if cfg!(windows) { ".lnk" } else { "" }
}

fn metadata(path: &Path) -> io::Result<Option<Metadata>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => Ok(Some(metadata)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

fn metadata_for_payload(path: &Path) -> io::Result<Option<Metadata>> {
  let info = metadata(path)?;
  if info.as_ref().is_some_and(|info| {
    if cfg!(target_os = "linux") {
      !private_fs::is_owned_file(info)
    } else {
      !private_fs::is_directory(info)
    }
  }) {
    return Err(invalid("unsafe desktop payload"));
  }
  Ok(info)
}

fn ensure_directory(path: &Path) -> io::Result<()> {
  match private_fs::create_directory(path) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
    Err(error) => return Err(error),
  }
  if !private_fs::is_directory(&fs::symlink_metadata(path)?) {
    return Err(invalid(
      "desktop storage must be an owned directory without links",
    ));
  }
  Ok(())
}

fn clean_abandoned_staging(root: &Path) -> io::Result<()> {
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let name = entry.file_name();
    let abandoned = name
      .to_str()
      .and_then(|name| name.strip_prefix("desktop-"))
      .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok());
    if abandoned {
      let path = entry.path();
      if !private_fs::is_directory(&fs::symlink_metadata(&path)?) {
        return Err(invalid("unsafe abandoned desktop staging directory"));
      }
      fs::remove_dir_all(path)?;
    }
  }
  private_fs::sync_directory(root)
}

fn ensure_subdirectories<const COUNT: usize>(
  root: &Path,
  names: [&str; COUNT],
) -> io::Result<PathBuf> {
  let mut path = root.to_owned();
  for name in names {
    path.push(name);
    ensure_directory(&path)?;
  }
  Ok(path)
}

pub(super) fn read_metadata(path: &Path, digest: &Digest) -> io::Result<DesktopMetadata> {
  use std::io::Read as _;
  let file = private_fs::options()
    .read(true)
    .open(path.join("desktop.json"))?;
  if !private_fs::is_private_file(&file.metadata()?) {
    return Err(invalid("desktop metadata must be a private owned file"));
  }
  let mut bytes = Vec::new();
  file.take(65537).read_to_end(&mut bytes)?;
  if bytes.len() > 65536 {
    return Err(invalid("desktop metadata exceeds its size limit"));
  }
  let metadata: DesktopMetadata = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
  metadata.validate(digest)?;
  Ok(metadata)
}

pub(super) fn status_payload(root: &Path, digest: &Digest) -> io::Result<Option<PathBuf>> {
  let names = ["store", "desktop", digest.as_str()];
  let Some(object) = existing_subdirectories(root, &names)? else {
    return Ok(None);
  };
  let payload = object.join(super::payload_name());
  Ok(metadata_for_payload(&payload)?.map(|_| payload))
}

pub(super) fn cached_metadata(root: &Path, digest: &Digest) -> io::Result<Option<DesktopMetadata>> {
  let names = ["cache", "downloads", "desktop", digest.as_str()];
  existing_subdirectories(root, &names)?
    .map(|directory| read_metadata(&directory, digest))
    .transpose()
}

fn existing_subdirectories(root: &Path, names: &[&str]) -> io::Result<Option<PathBuf>> {
  let mut path = root.to_owned();
  for name in std::iter::once("").chain(names.iter().copied()) {
    if !name.is_empty() {
      path.push(name);
    }
    match metadata(&path)? {
      Some(info) if private_fs::is_directory(&info) => {}
      Some(_) => return Err(invalid("unsafe desktop storage directory")),
      None => return Ok(None),
    }
  }
  Ok(Some(path))
}

fn write_metadata(path: &Path, metadata: &DesktopMetadata) -> io::Result<()> {
  write_metadata_file(&path.join("desktop.json"), metadata)
}

fn write_metadata_file(path: &Path, metadata: &DesktopMetadata) -> io::Result<()> {
  let mut output = private_fs::options()
    .create_new(true)
    .write(true)
    .open(path)?;
  serde_json::to_writer(&mut output, metadata).map_err(io::Error::other)?;
  writeln!(output)?;
  output.sync_all()
}

fn cache_package(directory: &Path, digest: &Digest) -> io::Result<Option<CachedPackage>> {
  let Some(info) = metadata(directory)? else {
    return Ok(None);
  };
  if !private_fs::is_directory(&info) {
    return Err(invalid("unsafe cached desktop package directory"));
  }
  Ok(Some(CachedPackage {
    metadata: read_metadata(directory, digest)?,
    archive: directory.join("package"),
  }))
}
