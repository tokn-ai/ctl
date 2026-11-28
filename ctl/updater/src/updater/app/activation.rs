use super::{
  CachedPackage, DesktopMetadata, DesktopNaming, DesktopPayload, Digest, app_destination, invalid,
};
#[cfg(target_os = "macos")]
use super::{Signing, SigningMode};
#[cfg(target_os = "macos")]
use ctl_client::remote_bundle::desktop::DesktopTarget;
use ctl_core::component_update::BundleChannel;
#[cfg(target_os = "macos")]
use ctl_core::installations::DesktopSource;
use ctl_core::installations::{DesktopSelection, Installations};
use ctl_core::private_fs;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
  collections::BTreeSet,
  fs::{self, DirEntry, File, Metadata},
  io::{self, Read as _, Write as _},
  path::{Path, PathBuf},
};

const CACHE_REFERENCES: &str = "references.json";
const SIGNED_REFERENCE: &str = "signed.json";

fn signing_reference(signing: super::Signing) -> &'static str {
  match signing {
    super::Signing::Provisioned => "provisioned.json",
    super::Signing::AdHoc => "ad-hoc.json",
    _ => SIGNED_REFERENCE,
  }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheReferences {
  stable: Option<Digest>,
  nightly: Option<Digest>,
}

impl CacheReferences {
  fn channel_mut(&mut self, channel: BundleChannel) -> &mut Option<Digest> {
    match channel {
      BundleChannel::Stable => &mut self.stable,
      BundleChannel::Nightly => &mut self.nightly,
    }
  }

  fn validate(&self) -> io::Result<()> {
    for digest in [&self.stable, &self.nightly].into_iter().flatten() {
      Digest::parse(digest.as_str())?;
    }
    Ok(())
  }
}

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
    status_payload(&self.installations, digest)
  }

  pub(super) fn cached(&self, digest: &Digest) -> io::Result<Option<CachedPackage>> {
    let directory = self.downloads.join(digest.as_str());
    cache_package(&directory, digest)
  }

  pub(super) fn cache(&self, package: &CachedPackage) -> io::Result<()> {
    self.cache_package(package)?;
    self.remember_package(&package.metadata)
  }

  fn cache_package(&self, package: &CachedPackage) -> io::Result<()> {
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
    let temporary = self
      .staging
      .join(format!("download-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&temporary)?;
    let archive = temporary.join("package");
    super::copy_package(&package.archive, &archive)?;
    if super::package_digest(&archive)? != package.metadata.content_id {
      return Err(invalid(
        "desktop archive changed while being copied to the cache",
      ));
    }
    write_metadata(&temporary, &package.metadata)?;
    private_fs::sync_directory(&temporary)?;
    let previous = self.staging.join(format!(
      "discarded-cache-{}",
      package.metadata.content_id.as_str()
    ));
    if cached.is_some() {
      // Replace a damaged cache entry only once a verified copy is staged.
      fs::rename(&directory, &previous)?;
    }
    if let Err(error) = fs::rename(&temporary, &directory) {
      if cached.is_some() {
        fs::rename(previous, &directory)?;
      }
      return Err(error);
    }
    private_fs::sync_directory(&self.downloads)
  }

  fn references(&self) -> io::Result<CacheReferences> {
    let references: CacheReferences =
      match read_private_json(&self.downloads.join(CACHE_REFERENCES)) {
        Ok(references) => references,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
          return Ok(CacheReferences::default());
        }
        Err(error) => return Err(error),
      };
    references.validate()?;
    Ok(references)
  }

  fn write_reference(
    &self,
    directory: &Path,
    filename: &str,
    value: &impl Serialize,
  ) -> io::Result<()> {
    let staged = self
      .staging
      .join(format!("reference-{}", uuid::Uuid::new_v4()));
    write_private_json(&staged, value)?;
    fs::rename(staged, directory.join(filename))?;
    private_fs::sync_directory(directory)
  }

  fn signed_reference(&self, source: &Digest, filename: &str) -> io::Result<Option<Digest>> {
    let directory = self.downloads.join(source.as_str());
    let Some(info) = metadata(&directory)? else {
      return Ok(None);
    };
    if !private_fs::is_directory(&info) {
      return Err(invalid("unsafe cached desktop package directory"));
    }
    let signed: Digest = match read_private_json(&directory.join(filename)) {
      Ok(signed) => signed,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    Digest::parse(signed.as_str())?;
    if &signed == source {
      return Err(invalid(
        "desktop signing cache refers to its input as the signed output",
      ));
    }
    Ok(Some(signed))
  }

  fn remember_package(&self, metadata: &DesktopMetadata) -> io::Result<()> {
    if let Some(source) = &metadata.signed_from {
      let reference = signing_reference(metadata.signing);
      if self.signed_reference(source, reference)?.as_ref() == Some(&metadata.content_id) {
        return Ok(());
      }
      let directory = self.downloads.join(source.as_str());
      if directory.is_dir() {
        return self.write_reference(&directory, reference, &metadata.content_id);
      }
      // A signed archive is still usable for repair if its original input was removed.
      return Ok(());
    }
    if let Some(channel) = metadata
      .provenance
      .as_ref()
      .and_then(|provenance| provenance.channel)
    {
      let mut references = self.references()?;
      let selected = references.channel_mut(channel);
      if selected.as_ref() == Some(&metadata.content_id) {
        return Ok(());
      }
      *selected = Some(metadata.content_id.clone());
      self.write_reference(&self.downloads, CACHE_REFERENCES, &references)?;
    }
    Ok(())
  }

  #[cfg(target_os = "macos")]
  pub(super) fn signing_package(
    &self,
    channel: Option<BundleChannel>,
    version: Option<&str>,
    target: DesktopTarget,
  ) -> io::Result<CachedPackage> {
    let selected = self.installations.desktop_selection()?;
    let package = match (channel, version) {
      (Some(channel), None) => self.package_for_channel(channel, selected.as_ref())?,
      (None, Some(version)) => self.package_for_version(version, selected.as_ref())?,
      (None, None) => self.active_signing_package(selected.as_ref())?,
      _ => return Err(invalid("choose a desktop signing channel or version")),
    };
    if package.metadata.target_triple != target.triple() {
      return Err(invalid(
        "cached desktop package does not match the signing target",
      ));
    }
    let digest = super::package_digest(&package.archive).map_err(|error| {
      if error.kind() == io::ErrorKind::NotFound {
        io::Error::new(
          io::ErrorKind::NotFound,
          "The selected desktop archive is missing. Install that build before signing.",
        )
      } else {
        error
      }
    })?;
    if digest != package.metadata.content_id {
      return Err(invalid(
        "cached desktop package checksum does not match its receipt",
      ));
    }
    Ok(package)
  }

  #[cfg(target_os = "macos")]
  fn active_signing_package(
    &self,
    selected: Option<&DesktopSelection>,
  ) -> io::Result<CachedPackage> {
    let found = selected
      .map(|selected| Digest::parse(&selected.content_id).and_then(|digest| self.cached(&digest)))
      .transpose()?
      .flatten();
    let Some(package) = found else {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "No active local development runtime or cached desktop build. Install the desktop with ctlup install app or select a cached build with --version.",
      ));
    };
    if !matches!(
      package.metadata.signing,
      Signing::Signed | Signing::Unsigned | Signing::Provisioned | Signing::AdHoc
    ) {
      return Err(invalid("active desktop is not a published macOS build"));
    }
    Ok(package)
  }

  #[cfg(target_os = "macos")]
  fn package_for_channel(
    &self,
    channel: BundleChannel,
    selected: Option<&DesktopSelection>,
  ) -> io::Result<CachedPackage> {
    let mut references = self.references()?;
    let indexed = references.channel_mut(channel).as_ref();
    let legacy = selected
      .filter(|selected| matches!(&selected.source, DesktopSource::Channel { channel: tracked } if tracked == channel.name()))
      .map(|selected| Digest::parse(&selected.content_id))
      .transpose()?;
    let found = indexed
      .or(legacy.as_ref())
      .map(|digest| self.cached(digest))
      .transpose()?
      .flatten();
    let Some(package) = found else {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
          "No {} desktop package is cached. Install the desktop from that channel first.",
          channel.name()
        ),
      ));
    };
    let provenance = package.metadata.provenance.as_ref();
    if !official_package(&package.metadata)
      || (indexed.is_some()
        && provenance.and_then(|provenance| provenance.channel) != Some(channel))
      || provenance
        .and_then(|provenance| provenance.channel)
        .is_some_and(|recorded| recorded != channel)
    {
      return Err(invalid(
        "cached desktop package does not belong to the signing channel",
      ));
    }
    Ok(package)
  }

  #[cfg(target_os = "macos")]
  fn package_for_version(
    &self,
    version: &str,
    selected: Option<&DesktopSelection>,
  ) -> io::Result<CachedPackage> {
    let legacy = selected.filter(|selected| {
      matches!(
        selected.source,
        DesktopSource::Channel { .. } | DesktopSource::Release { .. }
      )
    });
    let mut found: Option<CachedPackage> = None;
    for entry in fs::read_dir(&self.downloads)? {
      let Some(digest) = cache_entry_digest(&entry?) else {
        continue;
      };
      let Some(package) = self.cached(&digest)? else {
        continue;
      };
      if package.metadata.bundle_id.as_deref() != Some(version)
        || !official_package(&package.metadata)
        || !(package
          .metadata
          .provenance
          .as_ref()
          .is_some_and(|provenance| provenance.channel.is_some())
          || legacy.is_some_and(|selected| selected.content_id == digest.as_str()))
      {
        continue;
      }
      if let Some(previous) = &found {
        if previous.metadata.publication_id != package.metadata.publication_id
          || previous.metadata.provenance != package.metadata.provenance
          || previous.metadata.signing == package.metadata.signing
        {
          return Err(invalid(
            "multiple cached desktop packages match this version",
          ));
        }
        if previous.metadata.signing == Signing::Signed {
          continue;
        }
      }
      found = Some(package);
    }
    found.ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        format!("No desktop package for {version} is cached. Install that build first."),
      )
    })
  }

  #[cfg(target_os = "macos")]
  pub(super) fn cached_signed_derivative(
    &self,
    source: &CachedPackage,
    mode: SigningMode,
  ) -> io::Result<Option<CachedPackage>> {
    let signing = Signing::from(mode);
    let Some(derived) =
      self.signed_reference(&source.metadata.content_id, signing_reference(signing))?
    else {
      return Ok(None);
    };
    let Some(package) = self.cached(&derived)? else {
      return Ok(None);
    };
    let metadata = &package.metadata;
    if metadata.signing != signing
      || metadata.signed_from.as_ref() != Some(&source.metadata.content_id)
      || metadata.target_triple != source.metadata.target_triple
      || metadata.app_version != source.metadata.app_version
      || metadata.bundle_id != source.metadata.bundle_id
      || metadata.publication_id != source.metadata.publication_id
      || metadata.provenance != source.metadata.provenance
    {
      return Ok(None);
    }
    match super::package_digest(&package.archive) {
      Ok(digest) if digest == derived => Ok(Some(package)),
      Ok(_) => Ok(None),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
      Err(error) => Err(error),
    }
  }

  pub(super) fn activate(
    &self,
    prepared: &DesktopPayload,
    expected: Option<&DesktopSelection>,
    selected: &DesktopSelection,
    label: Option<&str>,
  ) -> io::Result<bool> {
    // The desktop lock serializes launch entry updates. Resolve the existing
    // target before publish_desktop takes the installation registry lock.
    self.managed_entry()?;
    let (launch, reused) =
      self
        .installations
        .publish_desktop(expected, selected, label, |object| {
          self.publish_payload(prepared, object)
        })?;
    launch.publish()?;
    Ok(reused)
  }

  fn publish_payload(
    &self,
    prepared: &DesktopPayload,
    object: &Path,
  ) -> io::Result<(LaunchEntry, bool)> {
    let payload = object.join(super::payload_name());
    let previous = self.staging.join("replaced-payload");
    if object_payload(object)?.is_some() && super::payload_matches(&payload, &prepared.path)? {
      let metadata = read_metadata(object, &prepared.metadata.content_id)?;
      self.refresh_metadata(object, &prepared.metadata, &metadata)?;
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
    let replaced = if let Some(info) = metadata(object)? {
      if !private_fs::is_directory(&info) {
        return Err(invalid("unsafe desktop payload directory"));
      }
      #[cfg(target_os = "macos")]
      if payload.is_dir() && super::macos::is_running(&payload)? {
        return Err(io::Error::other(
          "Quit the ctmux desktop app before repairing its current build.",
        ));
      }
      fs::rename(object, &previous)?;
      true
    } else {
      false
    };
    if let Err(error) = fs::rename(&staged, object) {
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
      self.managed_entry()?;
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
    let references = self.references()?;
    let mut retained = BTreeSet::new();
    let mut sources = Vec::from_iter(
      [references.stable, references.nightly]
        .into_iter()
        .flatten(),
    );
    if let Some(selected) = selected {
      let digest = Digest::parse(&selected.content_id)?;
      retained.insert(selected.content_id.clone());
      if let Some(source) = self
        .cached(&digest)?
        .and_then(|package| package.metadata.signed_from)
      {
        sources.push(source);
      }
    }
    for source in sources {
      for reference in [SIGNED_REFERENCE, "provisioned.json", "ad-hoc.json"] {
        if let Some(signed) = self.signed_reference(&source, reference)? {
          retained.insert(signed.as_str().to_owned());
        }
      }
      retained.insert(source.as_str().to_owned());
    }
    for entry in fs::read_dir(&self.downloads)? {
      let entry = entry?;
      let Some(digest) = cache_entry_digest(&entry) else {
        continue;
      };
      if retained.contains(digest.as_str()) {
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
    let managed = if info.file_type().is_symlink() {
      let target = fs::read_link(destination)?;
      match relative_link_object(&target, &self.store, self.installations.root()) {
        Some(name) => self.installations.is_desktop_object_name(name)?,
        None => false,
      }
    } else {
      false
    };
    #[cfg(windows)]
    let managed = private_fs::is_owned_file(&info)
      && super::shortcut::managed(destination, &self.store, &self.installations)?;
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
    write_private_json(&temporary, desired)?;
    fs::rename(temporary, directory.join("desktop.json"))?;
    private_fs::sync_directory(directory)
  }

  fn prepare_launch_entry(&self, payload: &Path) -> io::Result<LaunchEntry> {
    let destination = self.destination();
    let staged = if cfg!(windows) {
      self.staging.join("ctmux.lnk")
    } else {
      // The archive may still contain the named desktop payload in staging.
      self
        .staging
        .join(format!(".ctmux-{}", uuid::Uuid::new_v4()))
    };
    let entry = LaunchEntry {
      destination: destination.to_owned(),
      staged,
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
fn relative_link_object<'a>(link: &'a Path, store: &Path, profile: &Path) -> Option<&'a str> {
  let relative_store = store.strip_prefix(profile).ok()?;
  let prefix = Path::new("..").join(relative_store);
  let tail = link.strip_prefix(prefix).ok()?;
  if tail.file_name()? != super::payload_name() {
    return None;
  }
  tail.parent()?.to_str()
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
  let metadata: DesktopMetadata = read_private_json(&path.join("desktop.json"))?;
  metadata.validate(digest)?;
  Ok(metadata)
}

fn read_private_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
  let file = private_fs::options().read(true).open(path)?;
  if !private_fs::is_private_file(&file.metadata()?) {
    return Err(invalid("desktop metadata must be a private owned file"));
  }
  let mut bytes = Vec::new();
  file.take(65537).read_to_end(&mut bytes)?;
  if bytes.len() > 65536 {
    return Err(invalid("desktop metadata exceeds its size limit"));
  }
  serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub(super) fn status_payload(
  installations: &Installations,
  digest: &Digest,
) -> io::Result<Option<PathBuf>> {
  object_payload(&installations.desktop_object_path(digest.as_str())?)
}

fn object_payload(object: &Path) -> io::Result<Option<PathBuf>> {
  let Some(info) = metadata(object)? else {
    return Ok(None);
  };
  if !private_fs::is_directory(&info) {
    return Err(invalid("unsafe desktop payload directory"));
  }
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
  write_private_json(&path.join("desktop.json"), metadata)
}

fn write_private_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
  let mut output = private_fs::options()
    .create_new(true)
    .write(true)
    .open(path)?;
  serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
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
  let metadata = read_metadata(directory, digest)?;
  let naming = metadata
    .provenance
    .as_ref()
    .map_or_else(DesktopNaming::default, |provenance| DesktopNaming {
      installation_layout_version: provenance.installation_layout_version,
      source_revision: Some(provenance.source_revision.clone()),
      built_at: provenance.built_at.clone(),
    });
  Ok(Some(CachedPackage {
    metadata,
    archive: directory.join("package"),
    naming,
  }))
}

fn cache_entry_digest(entry: &DirEntry) -> Option<Digest> {
  Digest::parse(entry.file_name().to_str()?).ok()
}

#[cfg(target_os = "macos")]
fn official_package(metadata: &DesktopMetadata) -> bool {
  metadata.signed_from.is_none() && matches!(metadata.signing, Signing::Signed | Signing::Unsigned)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
  use super::*;
  use crate::updater::app::DesktopProvenance;

  struct Fixture(PathBuf);

  impl Fixture {
    fn new() -> Self {
      let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
          "test-desktop-signing-cache-{}",
          uuid::Uuid::new_v4()
        ));
      private_fs::create_directory(&root).unwrap();
      Self(root)
    }

    fn workspace(&self) -> Workspace {
      let [lock, store, downloads, staging, destination] =
        ["lock", "store", "downloads", "staging", "ctmux.app"].map(|name| self.0.join(name));
      for directory in [&store, &downloads, &staging] {
        private_fs::create_directory(directory).unwrap();
      }
      Workspace {
        installations: Installations::new(&self.0),
        store,
        downloads,
        staging,
        destination,
        _lock: private_fs::options()
          .write(true)
          .create_new(true)
          .open(lock)
          .unwrap(),
      }
    }

    fn package(&self, name: &str, bundle_id: &str, channel: BundleChannel) -> CachedPackage {
      let archive = self.0.join(name);
      fs::write(&archive, name).unwrap();
      let metadata = DesktopMetadata {
        content_id: super::super::package_digest(&archive).unwrap(),
        bundle_id: Some(bundle_id.into()),
        app_version: Some("0.1.5".into()),
        publication_id: Some(format!("{bundle_id}-17-1")),
        signing: Signing::Unsigned,
        provenance: Some(DesktopProvenance {
          channel: Some(channel),
          source_revision: "a".repeat(40),
          installation_layout_version: Some(7),
          built_at: Some("2026-01-01T00:00:00Z".into()),
        }),
        ..DesktopMetadata::provided(DesktopTarget::native().unwrap())
      };
      CachedPackage {
        archive,
        metadata,
        naming: DesktopNaming::default(),
      }
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      fs::remove_dir_all(&self.0).unwrap();
    }
  }

  #[test]
  fn retains_channel_packages_and_their_signed_derivatives() {
    let fixture = Fixture::new();
    let work = fixture.workspace();
    let old = fixture.package("old archive", "0.1.4", BundleChannel::Stable);
    let stable = fixture.package("stable archive", "0.1.5", BundleChannel::Stable);
    let nightly = fixture.package(
      "nightly archive",
      "0.1.5-nightly.20260101+gaaaaaaaaaaaa",
      BundleChannel::Nightly,
    );
    let mut signed = fixture.package("signed archive", "0.1.5", BundleChannel::Stable);
    signed.metadata.signing = Signing::Signed;
    signed.metadata.signed_from = Some(stable.metadata.content_id.clone());
    let mut ad_hoc = fixture.package("ad hoc archive", "0.1.5", BundleChannel::Stable);
    ad_hoc.metadata.signing = Signing::AdHoc;
    ad_hoc.metadata.signed_from = Some(stable.metadata.content_id.clone());
    for package in [&old, &stable, &nightly, &signed, &ad_hoc] {
      work.cache(package).unwrap();
    }
    work.prune_downloads(None).unwrap();
    assert!(work.cached(&old.metadata.content_id).unwrap().is_none());
    for (channel, expected) in [
      (BundleChannel::Stable, &stable),
      (BundleChannel::Nightly, &nightly),
    ] {
      let found = work.package_for_channel(channel, None).unwrap();
      assert_eq!(found.metadata.content_id, expected.metadata.content_id);
      assert_eq!(found.naming.installation_layout_version, Some(7));
    }
    let cached = work
      .cached_signed_derivative(&stable, SigningMode::DeveloperId)
      .unwrap()
      .unwrap();
    assert_eq!(cached.metadata.content_id, signed.metadata.content_id);
    let cached_ad_hoc = work
      .cached_signed_derivative(&stable, SigningMode::AdHoc)
      .unwrap()
      .unwrap();
    assert_eq!(
      cached_ad_hoc.metadata.content_id,
      ad_hoc.metadata.content_id
    );
    let selected = DesktopSelection {
      content_id: cached.metadata.content_id.as_str().to_owned(),
      source: DesktopSource::Channel {
        channel: BundleChannel::Stable.name().to_owned(),
      },
    };
    let active = work.active_signing_package(Some(&selected)).unwrap();
    assert_eq!(active.metadata.content_id, cached.metadata.content_id);
    fs::write(cached.archive, "damaged signed archive").unwrap();
    assert!(
      work
        .cached_signed_derivative(&stable, SigningMode::DeveloperId)
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn changed_bytes_do_not_publish_a_cache_entry_or_channel_reference() {
    let fixture = Fixture::new();
    let work = fixture.workspace();
    let package = fixture.package("stable archive", "0.1.5", BundleChannel::Stable);
    fs::write(&package.archive, "damaged unsigned archive").unwrap();
    assert!(work.cache(&package).is_err());
    assert!(work.cached(&package.metadata.content_id).unwrap().is_none());
    assert!(work.references().unwrap().stable.is_none());
  }

  #[test]
  fn exact_version_uses_the_bundle_id_and_rejects_conflicting_publications() {
    let fixture = Fixture::new();
    let work = fixture.workspace();
    let stable = fixture.package("stable archive", "0.1.5", BundleChannel::Stable);
    let nightly = fixture.package(
      "nightly archive",
      "0.1.5-nightly.20260101+gaaaaaaaaaaaa",
      BundleChannel::Nightly,
    );
    work.cache(&stable).unwrap();
    work.cache(&nightly).unwrap();
    assert_eq!(
      work
        .package_for_version("0.1.5", None)
        .unwrap()
        .metadata
        .content_id,
      stable.metadata.content_id
    );
    let mut conflicting = fixture.package("other stable archive", "0.1.5", BundleChannel::Stable);
    conflicting.metadata.publication_id = Some("0.1.5-18-1".into());
    work.cache(&conflicting).unwrap();
    let result = work.package_for_version("0.1.5", None);
    assert!(matches!(result, Err(error) if error.to_string().contains("multiple cached")));
  }
}
