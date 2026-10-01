//! Atomic catalog snapshots shared by desktop and CLI writers.
//!
//! Writers use `workspace.lock` beside the catalog, including the desktop's
//! workspace migrations. Revisions are hashes of decoded content, so external
//! edits cannot hide behind an unchanged stored revision.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{HostCatalogDocument, HostCatalogSnapshot, HostError};

const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Read a snapshot without creating directories or lock files.
///
/// # Errors
/// Rejects unreadable, oversized, unsupported, or invalid saved data.
pub fn load(path: &Path) -> Result<HostCatalogSnapshot, HostError> {
  regular_file_or_absent(path).map_err(io_error)?;
  let file = match File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      return Ok(HostCatalogSnapshot::default());
    }
    Err(error) => return Err(io_error(error)),
  };
  let mut bytes = Vec::new();
  file
    .take(MAX_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(io_error)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(too_large());
  }
  let mut snapshot: HostCatalogSnapshot = serde_json::from_slice(&bytes).map_err(|error| {
    HostError::new(
      "hosts_unreadable",
      format!("Could not read the host catalog; its file has been preserved: {error}"),
    )
  })?;
  snapshot.document.validate()?;
  if snapshot.revision.as_ref().is_none_or(String::is_empty) {
    return Err(HostError::new(
      "hosts_invalid",
      "The host catalog has no revision. Its file has not been changed.",
    ));
  }
  snapshot.revision = Some(revision(&snapshot.document)?);
  Ok(snapshot)
}

/// Compare and save a complete catalog while holding the shared writer lock.
///
/// # Errors
/// Rejects invalid data, stale revisions, pending legacy migration, or I/O failure.
pub fn update(
  path: &Path,
  expected_revision: Option<&str>,
  document: HostCatalogDocument,
) -> Result<HostCatalogSnapshot, HostError> {
  document.validate()?;
  let directory = parent(path);
  let _lock = lock_directory(directory)?;
  // Legacy desktop workspaces still own their host definitions. Let the
  // desktop finish its existing migration before allowing independent edits.
  let workspace = directory.join("workspace.json");
  regular_file_or_absent(&workspace).map_err(io_error)?;
  let workspace_file = match File::open(&workspace) {
    Ok(file) => Some(file),
    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
    Err(error) => return Err(io_error(error)),
  };
  if let Some(file) = workspace_file {
    let mut bytes = Vec::new();
    file
      .take(MAX_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(io_error)?;
    if bytes.len() as u64 > MAX_BYTES {
      return Err(too_large());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
      HostError::new(
        "hosts_migration_required",
        "Open the desktop app to resolve the workspace before editing hosts.",
      )
    })?;
    if value["document"]["schema_version"] != 8 {
      return Err(HostError::new(
        "hosts_migration_required",
        "Open the desktop app to migrate the workspace before editing hosts.",
      ));
    }
  }
  let current = load(path)?;
  if current.revision.as_deref() != expected_revision {
    return Err(HostError::new(
      "hosts_conflict",
      "Another app instance or editor changed the hosts. Reload before saving further changes.",
    ));
  }
  let snapshot = HostCatalogSnapshot {
    revision: Some(revision(&document)?),
    document,
  };
  persist_under_lock(path, &snapshot)?;
  Ok(snapshot)
}

/// Acquire the lock used by all catalog and desktop workspace writers.
///
/// # Errors
/// Returns an error for inaccessible directories or unsafe lock paths.
pub fn lock_directory(directory: &Path) -> Result<File, HostError> {
  let mut builder = fs::DirBuilder::new();
  builder.recursive(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder.create(directory).map_err(io_error)?;
  let path = directory.join("workspace.lock");
  regular_file_or_absent(&path).map_err(io_error)?;
  let mut options = OpenOptions::new();
  options.create(true).read(true).write(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  let file = options.open(path).map_err(io_error)?;
  file.lock().map_err(io_error)?;
  Ok(file)
}

/// Save an already-validated snapshot while the caller holds `workspace.lock`.
/// This entry point lets desktop migration commit under its existing lock.
///
/// # Errors
/// Rejects invalid or oversized data and reports atomic-write failures.
pub fn persist_under_lock(path: &Path, snapshot: &HostCatalogSnapshot) -> Result<(), HostError> {
  snapshot.document.validate()?;
  let bytes = serde_json::to_vec_pretty(snapshot).map_err(json_error)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(too_large());
  }
  regular_file_or_absent(path).map_err(io_error)?;
  let temporary = TemporaryFile(parent(path).join(format!(".hosts-{}.tmp", uuid::Uuid::new_v4())));
  let mut options = OpenOptions::new();
  options.create_new(true).write(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  let mut file = options.open(&temporary.0).map_err(io_error)?;
  file.write_all(&bytes).map_err(io_error)?;
  file.sync_all().map_err(io_error)?;
  drop(file);
  fs::rename(&temporary.0, path).map_err(io_error)?;
  #[cfg(unix)]
  File::open(parent(path))
    .and_then(|file| file.sync_all())
    .map_err(io_error)?;
  Ok(())
}

/// Hash the same canonical JSON representation used by desktop revisions.
///
/// # Errors
/// Returns a serialization error if the document cannot be encoded.
pub fn revision(document: &HostCatalogDocument) -> Result<String, HostError> {
  Ok(format!(
    "sha256:{:x}",
    Sha256::digest(serde_json::to_vec(document).map_err(json_error)?)
  ))
}

fn parent(path: &Path) -> &Path {
  path
    .parent()
    .filter(|path| !path.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."))
}

fn regular_file_or_absent(path: &Path) -> io::Result<()> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.is_file() => Ok(()),
    Ok(_) => Err(io::Error::other(
      "Catalog paths must be regular files, not symlinks or directories.",
    )),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

struct TemporaryFile(PathBuf);
impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ignored = fs::remove_file(&self.0);
  }
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> HostError {
  HostError::new(
    "hosts_io_failed",
    format!("Could not access the host catalog: {error}"),
  )
}
#[allow(clippy::needless_pass_by_value)]
fn json_error(error: serde_json::Error) -> HostError {
  HostError::new("hosts_invalid", error.to_string())
}
fn too_large() -> HostError {
  HostError::new(
    "hosts_too_large",
    "The host catalog exceeds its size limit. Its file has not been changed.",
  )
}
