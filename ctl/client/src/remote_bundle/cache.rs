//! Immutable, verified per-build cache entries with atomic publication.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use super::{
  BUNDLE_SET_FILE, BundleSet, ComponentBuildInfo, Error, MAX_BUNDLE_BYTES, MAX_BUNDLE_SET_BYTES,
  VerifiedBundle, validate_target,
};

/// One remote target at an exact clean client revision. Construction performs
/// no filesystem work. Run `load` and `store` on a blocking worker.
#[derive(Debug, Clone)]
pub struct BundleCacheEntry {
  root: PathBuf,
  revision_directory: PathBuf,
  directory: PathBuf,
  target: String,
  expected: ComponentBuildInfo,
}

impl BundleCacheEntry {
  /// Selects `root/<full source revision>/<target>/` without creating it.
  ///
  /// # Errors
  /// Rejects unsupported targets and unidentified or dirty client builds.
  pub fn new(root: &Path, target: &str, expected: &ComponentBuildInfo) -> Result<Self, Error> {
    validate_target(target)?;
    let revision = expected.source_revision.as_deref().ok_or_else(|| {
      Error::Stale("remote bundle caching requires an identified clean client build".into())
    })?;
    if !expected.is_valid()
      || expected.dirty
      || revision.len() != 40
      || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
      return Err(Error::Stale(
        "remote bundle caching requires an identified clean client build".into(),
      ));
    }
    if root.as_os_str().is_empty()
      || root
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
      return Err(Error::Invalid("invalid remote bundle cache root".into()));
    }
    let revision_directory = root.join(revision);
    Ok(Self {
      root: root.to_owned(),
      directory: revision_directory.join(target),
      revision_directory,
      target: target.into(),
      expected: expected.clone(),
    })
  }

  /// Rechecks the stored version, exact source revision, and archive SHA-256.
  /// Missing, truncated, or damaged contents are a cache miss.
  ///
  /// # Errors
  /// Reports inaccessible paths or unsafe directories, links, and special files.
  pub fn load(&self) -> Result<Option<VerifiedBundle>, Error> {
    for path in [&self.root, &self.revision_directory, &self.directory] {
      if !existing_directory(path)? {
        return Ok(None);
      }
    }
    match self.read_contents() {
      Ok(bundle) => Ok(Some(bundle)),
      Err(Error::Invalid(_) | Error::Stale(_) | Error::UnsupportedTarget(_)) => Ok(None),
      Err(Error::Io(error))
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
        ) =>
      {
        Ok(None)
      }
      Err(error) => Err(error),
    }
  }

  fn read_contents(&self) -> Result<VerifiedBundle, Error> {
    let bytes = read_private_file(&self.directory.join(BUNDLE_SET_FILE), MAX_BUNDLE_SET_BYTES)?;
    let manifest = BundleSet::parse(&bytes, &self.expected.version)?;
    manifest.verify_revision(&self.expected)?;
    let archive = read_private_file(
      &self.directory.join(manifest.archive_name(&self.target)?),
      MAX_BUNDLE_BYTES,
    )?;
    manifest.verify_archive(&self.target, archive, bytes)
  }

  /// Revalidates a bundle before publishing a complete immutable entry. The
  /// publication lock covers filesystem work only; callers download first.
  ///
  /// # Errors
  /// Rejects modified bundle data, unsafe paths, and filesystem failures.
  pub fn store(&self, bundle: &VerifiedBundle) -> Result<(), Error> {
    self.verify_bundle(bundle)?;
    ensure_directory(&self.root)?;
    ensure_directory(&self.revision_directory)?;
    let _lock = PublicationLock::acquire(
      &self
        .revision_directory
        .join(format!("{}.lock", self.target)),
    )?;
    // Another process may have published the same verified entry while our
    // download was running. Leave that immutable winner in place.
    if self.load()?.is_some() {
      return Ok(());
    }
    let stage = TemporaryDirectory::new(&self.revision_directory, &self.target)?;
    write_private_file(&stage.0.join(BUNDLE_SET_FILE), &bundle.manifest)?;
    write_private_file(&stage.0.join(&bundle.file_name), &bundle.archive)?;
    sync_directory(&stage.0)?;
    self.publish(&stage)?;
    sync_directory(&self.revision_directory)?;
    Ok(())
  }

  fn verify_bundle(&self, bundle: &VerifiedBundle) -> Result<(), Error> {
    let manifest = BundleSet::parse(&bundle.manifest, &self.expected.version)?;
    manifest.verify_revision(&self.expected)?;
    if bundle.app_version != manifest.app_version
      || bundle.bundle_id != manifest.bundle_id
      || bundle.git_revision != manifest.git_revision
      || bundle.file_name != manifest.archive_name(&self.target)?
    {
      return Err(Error::Invalid(
        "cached remote bundle metadata disagrees with its validated manifest".into(),
      ));
    }
    manifest.verify_archive_bytes(&self.target, &bundle.archive)?;
    if manifest.schema_version == 2 {
      super::compatibility::verify_archive(&manifest, &self.target, &bundle.archive)?;
    }
    Ok(())
  }

  fn publish(&self, stage: &TemporaryDirectory) -> Result<(), Error> {
    let displaced = if existing_directory(&self.directory)? {
      let path = unique_sibling(&self.revision_directory, &self.target, "corrupt");
      fs::rename(&self.directory, &path)?;
      Some(TemporaryDirectory(path))
    } else {
      None
    };
    if let Err(error) = fs::rename(&stage.0, &self.directory) {
      if let Some(displaced) = &displaced {
        let _ = fs::rename(&displaced.0, &self.directory);
      }
      return Err(error.into());
    }
    Ok(())
  }
}

/// Selects an exact clean-client entry first, then compatible schema-2 entries
/// in stable revision order. No directory is created and no live owner changes.
/// Dirty or unidentified clients may reuse independently verified schema-2 data.
///
/// # Errors
/// Rejects unsafe cache paths or inaccessible files. Damaged entries are misses.
pub fn read_compatible_cached_bundle(
  root: &Path,
  target: &str,
  expected: &ComponentBuildInfo,
) -> Result<Option<VerifiedBundle>, Error> {
  validate_target(target)?;
  if root.as_os_str().is_empty()
    || root
      .components()
      .any(|part| matches!(part, std::path::Component::ParentDir))
  {
    return Err(Error::Invalid("invalid remote bundle cache root".into()));
  }
  if !existing_directory(root)? {
    return Ok(None);
  }
  if !expected.dirty
    && expected.is_valid()
    && expected
      .source_revision
      .as_ref()
      .is_some_and(|revision| revision.len() == 40)
  {
    let exact = BundleCacheEntry::new(root, target, expected)?;
    if let Some(bundle) = exact.load()? {
      // Exact schema-1 candidates remain eligible. Schema-2 candidates must
      // satisfy today's required contracts even at the same source revision.
      let manifest = BundleSet::parse_intrinsic(&bundle.manifest)?;
      if manifest.schema_version == 1 || manifest.is_compatible(target)? {
        return Ok(Some(bundle));
      }
    }
  }
  let mut revisions = Vec::new();
  for entry in fs::read_dir(root)? {
    let entry = entry?;
    let name = entry.file_name();
    let Some(name) = name.to_str() else { continue };
    if name.len() == 40 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
      revisions.push(name.to_owned());
      if revisions.len() > 4096 {
        return Err(Error::Invalid(
          "remote bundle cache contains too many revisions".into(),
        ));
      }
    }
  }
  revisions.sort_by(|left, right| {
    let preferred = expected.source_revision.as_deref();
    (Some(left.as_str()) != preferred, left).cmp(&(Some(right.as_str()) != preferred, right))
  });
  for revision in revisions {
    let revision_directory = root.join(&revision);
    let directory = revision_directory.join(target);
    if !existing_directory(&revision_directory)? || !existing_directory(&directory)? {
      continue;
    }
    match read_compatible_entry(&directory, &revision, target) {
      Ok(Some(bundle)) => return Ok(Some(bundle)),
      Ok(None) | Err(Error::Invalid(_) | Error::Stale(_) | Error::UnsupportedTarget(_)) => {}
      Err(Error::Io(error))
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
        ) => {}
      Err(error) => return Err(error),
    }
  }
  Ok(None)
}

fn read_compatible_entry(
  directory: &Path,
  revision: &str,
  target: &str,
) -> Result<Option<VerifiedBundle>, Error> {
  let bytes = read_private_file(&directory.join(BUNDLE_SET_FILE), MAX_BUNDLE_SET_BYTES)?;
  let manifest = BundleSet::parse_intrinsic(&bytes)?;
  if manifest.git_revision != revision {
    return Err(Error::Invalid(
      "cached bundle source does not match its immutable revision path".into(),
    ));
  }
  if manifest.schema_version != 2 || !manifest.is_compatible(target)? {
    return Ok(None);
  }
  let archive = read_private_file(
    &directory.join(manifest.archive_name(target)?),
    MAX_BUNDLE_BYTES,
  )?;
  let bundle = manifest.verify_archive(target, archive, bytes)?;
  Ok(Some(bundle))
}

fn existing_directory(path: &Path) -> io::Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => {
      require_safe_directory(&metadata)?;
      Ok(true)
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn ensure_directory(path: &Path) -> io::Result<()> {
  if existing_directory(path)? {
    return Ok(());
  }
  if let Some(parent) = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
  {
    ensure_directory(parent)?;
  }
  match create_private_directory(path) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
    Err(error) => return Err(error),
  }
  existing_directory(path)?;
  Ok(())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
  let builder = fs::DirBuilder::new();
  #[cfg(unix)]
  let mut builder = builder;
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.create(path)
}

fn require_safe_directory(metadata: &Metadata) -> io::Result<()> {
  if !metadata.is_dir() || is_reparse_point(metadata) {
    return Err(unsafe_path());
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o022 != 0 {
      return Err(unsafe_path());
    }
  }
  Ok(())
}

fn require_private_file(metadata: &Metadata) -> io::Result<()> {
  if !metadata.is_file() || is_reparse_point(metadata) {
    return Err(unsafe_path());
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o077 != 0 {
      return Err(unsafe_path());
    }
  }
  Ok(())
}

fn is_reparse_point(metadata: &Metadata) -> bool {
  #[cfg(windows)]
  {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x0400 != 0
  }
  #[cfg(not(windows))]
  {
    metadata.file_type().is_symlink()
  }
}

fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600).custom_flags(i32::from_ne_bytes(
      (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC)
        .bits()
        .to_ne_bytes(),
    ));
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt as _;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  options
}

fn read_private_file(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
  require_private_file(&fs::symlink_metadata(path)?)?;
  let file = private_options().read(true).open(path)?;
  let metadata = file.metadata()?;
  require_private_file(&metadata)?;
  if metadata.len() > maximum as u64 {
    return Err(Error::Invalid(
      "remote bundle cache file is oversized".into(),
    ));
  }
  let mut bytes = Vec::new();
  file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > maximum {
    return Err(Error::Invalid(
      "remote bundle cache file is oversized".into(),
    ));
  }
  Ok(bytes)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
  let mut file = private_options().write(true).create_new(true).open(path)?;
  require_private_file(&file.metadata()?)?;
  file.write_all(bytes)?;
  file.sync_all()
}

struct PublicationLock(File);

impl PublicationLock {
  fn acquire(path: &Path) -> io::Result<Self> {
    match fs::symlink_metadata(path) {
      Ok(metadata) => require_private_file(&metadata)?,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
    let file = private_options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(path)?;
    require_private_file(&file.metadata()?)?;
    file.try_lock().map_err(|error| match error {
      fs::TryLockError::WouldBlock => io::Error::new(
        io::ErrorKind::WouldBlock,
        "another process is publishing this remote bundle cache entry",
      ),
      fs::TryLockError::Error(error) => error,
    })?;
    Ok(Self(file))
  }
}

impl Drop for PublicationLock {
  fn drop(&mut self) {
    let _ = self.0.unlock();
  }
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
  fn new(parent: &Path, target: &str) -> io::Result<Self> {
    let path = unique_sibling(parent, target, "partial");
    create_private_directory(&path)?;
    Ok(Self(path))
  }
}

impl Drop for TemporaryDirectory {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn unique_sibling(parent: &Path, target: &str, kind: &str) -> PathBuf {
  parent.join(format!(".{target}-{kind}-{}", uuid::Uuid::new_v4()))
}

fn sync_directory(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  {
    File::open(path)?.sync_all()
  }
  #[cfg(not(unix))]
  {
    let _ = path;
    Ok(())
  }
}

fn unsafe_path() -> io::Error {
  io::Error::new(
    io::ErrorKind::PermissionDenied,
    "remote bundle cache paths must be owned directories or private regular files, not links or special files",
  )
}

#[cfg(test)]
mod tests;
