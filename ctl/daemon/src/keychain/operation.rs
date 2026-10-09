//! Cross-process serialization for interactive Keychain operations.
//!
//! The lock file contains no credential data. The OS releases the lock if a
//! helper dies, so an import can safely reconcile abandoned metadata writes.

use super::Error;
use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Application-local status, deliberately outside the Security OSStatus range.
pub(super) const BUSY: i32 = -1_000_001;
const IO_ERROR: i32 = -36;
const INITIAL_REVISION: &str = "none";
const REVISION_FILE: &str = "credential-revision";

pub(super) struct Guard {
  file: File,
  directory: PathBuf,
}

impl Guard {
  /// Read the revocation revision while this operation owns the shared lock.
  pub(super) fn revision(&self) -> Result<String, Error> {
    revision_at(&self.directory)
  }
}

impl Drop for Guard {
  fn drop(&mut self) {
    // Closing only this descriptor can leave the lock held by a descriptor
    // inherited during a concurrent fork, until the child execs or exits.
    let _ = self.file.unlock();
  }
}

fn directory() -> Result<PathBuf, Error> {
  Ok(
    dirs::cache_dir()
      .ok_or_else(io_error)?
      .join("dev.tokn-ai.ctl.ctld")
      .join("keychain-operations"),
  )
}

pub(super) fn acquire() -> Result<Guard, Error> {
  let directory = directory()?;
  acquire_at(&directory, Duration::from_secs(30))
}

/// A passive check creates neither the directory nor the revision file.
pub(super) fn revision() -> Result<String, Error> {
  revision_at(&directory()?)
}

/// The caller must hold the cross-process operation lock throughout mutation.
/// Publish before changing any secret, including operations that later fail.
pub(super) fn advance_revision() -> Result<(), Error> {
  advance_revision_at(&directory()?)
}

fn valid_directory(directory: &Path) -> Result<bool, Error> {
  let metadata = match std::fs::symlink_metadata(directory) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    Err(_) => return Err(io_error()),
  };
  if !metadata.is_dir()
    || metadata.uid() != rustix::process::geteuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(io_error());
  }
  Ok(true)
}

fn revision_at(directory: &Path) -> Result<String, Error> {
  if !valid_directory(directory)? {
    return Ok(INITIAL_REVISION.into());
  }
  let file = match OpenOptions::new()
    .read(true)
    .custom_flags(
      i32::try_from((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits())
        .map_err(|_| io_error())?,
    )
    .open(directory.join(REVISION_FILE))
  {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
      return Ok(INITIAL_REVISION.into());
    }
    Err(_) => return Err(io_error()),
  };
  let metadata = file.metadata().map_err(|_| io_error())?;
  if !metadata.is_file()
    || metadata.uid() != rustix::process::geteuid().as_raw()
    || metadata.mode() & 0o077 != 0
    || metadata.nlink() != 1
    || metadata.len() != 32
  {
    return Err(io_error());
  }
  let mut revision = String::new();
  file
    .take(33)
    .read_to_string(&mut revision)
    .map_err(|_| io_error())?;
  if revision.len() != 32
    || !revision
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    || uuid::Uuid::parse_str(&revision).is_err()
  {
    return Err(io_error());
  }
  Ok(revision)
}

fn advance_revision_at(directory: &Path) -> Result<(), Error> {
  if !valid_directory(directory)? {
    return Err(io_error());
  }
  // Reject a symlink, nonprivate file, or malformed previous revision instead
  // of silently treating it as an untouched credential store.
  revision_at(directory)?;
  let revision = uuid::Uuid::new_v4().simple().to_string();
  let temporary = directory.join(format!(".credential-revision-{revision}"));
  let outcome = (|| {
    let mut file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&temporary)
      .map_err(|_| io_error())?;
    file
      .write_all(revision.as_bytes())
      .map_err(|_| io_error())?;
    file.sync_all().map_err(|_| io_error())?;
    std::fs::rename(&temporary, directory.join(REVISION_FILE)).map_err(|_| io_error())?;
    File::open(directory)
      .and_then(|file| file.sync_all())
      .map_err(|_| io_error())
  })();
  if outcome.is_err() {
    let _ = std::fs::remove_file(&temporary);
  }
  outcome
}

fn acquire_at(directory: &Path, deadline: Duration) -> Result<Guard, Error> {
  DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(directory)
    .map_err(|_| io_error())?;
  let metadata = std::fs::symlink_metadata(directory).map_err(|_| io_error())?;
  if !metadata.is_dir()
    || metadata.uid() != rustix::process::geteuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(io_error());
  }
  let file = OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .mode(0o600)
    .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(|_| io_error())?)
    .open(directory.join("operation.lock"))
    .map_err(|_| io_error())?;
  let metadata = file.metadata().map_err(|_| io_error())?;
  if !metadata.is_file()
    || metadata.uid() != rustix::process::geteuid().as_raw()
    || metadata.mode() & 0o077 != 0
    || metadata.nlink() != 1
  {
    return Err(io_error());
  }
  let started = Instant::now();
  loop {
    match file.try_lock() {
      Ok(()) => {
        return Ok(Guard {
          file,
          directory: directory.to_owned(),
        });
      }
      Err(TryLockError::WouldBlock) if started.elapsed() < deadline => {
        std::thread::sleep(Duration::from_millis(25));
      }
      Err(TryLockError::WouldBlock) => {
        return Err(Error(security_framework::base::Error::from_code(BUSY)));
      }
      Err(TryLockError::Error(_)) => return Err(io_error()),
    }
  }
}

fn io_error() -> Error {
  Error(security_framework::base::Error::from_code(IO_ERROR))
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::os::unix::fs::{PermissionsExt as _, symlink};

  struct Fixture(std::path::PathBuf);

  impl Fixture {
    fn new() -> Self {
      Self(std::env::temp_dir().join(format!("keychain-lock-fixture-{}", uuid::Uuid::new_v4())))
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn concurrent_operations_wait_and_releasing_the_owner_allows_retry() {
    let fixture = Fixture::new();
    let first = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    let error = acquire_at(&fixture.0, Duration::ZERO).err().unwrap();
    assert_eq!(error.0.code(), BUSY);
    assert_eq!(
      std::fs::read(fixture.0.join("operation.lock")).unwrap(),
      b""
    );
    drop(first);
    assert!(acquire_at(&fixture.0, Duration::ZERO).is_ok());
  }

  #[test]
  fn releasing_the_owner_unlocks_even_with_a_duplicated_descriptor() {
    let fixture = Fixture::new();
    let owner = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    // A concurrent process spawn can briefly inherit this open file description.
    let inherited = owner.file.try_clone().unwrap();
    drop(owner);
    let next = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    drop(inherited);
    drop(next);
  }

  #[test]
  fn lock_file_symlinks_are_rejected_without_changing_the_target() {
    let fixture = Fixture::new();
    DirBuilder::new().mode(0o700).create(&fixture.0).unwrap();
    let target = fixture.0.join("untouched");
    std::fs::write(&target, b"fixture").unwrap();
    symlink(&target, fixture.0.join("operation.lock")).unwrap();
    assert!(acquire_at(&fixture.0, Duration::ZERO).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"fixture");
  }

  #[test]
  fn passive_revision_is_initial_and_has_no_creation_side_effects() {
    let fixture = Fixture::new();
    assert_eq!(revision_at(&fixture.0).unwrap(), INITIAL_REVISION);
    assert!(!fixture.0.exists());
    let operation = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    assert_eq!(operation.revision().unwrap(), INITIAL_REVISION);
    assert!(!fixture.0.join(REVISION_FILE).exists());
  }

  #[test]
  fn revisions_are_private_atomic_and_change_for_every_mutation() {
    let fixture = Fixture::new();
    let operation = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    advance_revision_at(&fixture.0).unwrap();
    let first = operation.revision().unwrap();
    assert_ne!(first, INITIAL_REVISION);
    assert!(uuid::Uuid::parse_str(&first).is_ok());
    advance_revision_at(&fixture.0).unwrap();
    assert_ne!(operation.revision().unwrap(), first);
    assert_eq!(
      std::fs::metadata(fixture.0.join(REVISION_FILE))
        .unwrap()
        .mode()
        & 0o777,
      0o600
    );
    assert_eq!(std::fs::read_dir(&fixture.0).unwrap().count(), 2);
    assert_eq!(
      std::fs::read(fixture.0.join("operation.lock")).unwrap(),
      b""
    );
  }

  #[test]
  fn a_separate_operation_observes_revocation_after_the_locked_mutation() {
    let fixture = Fixture::new();
    let operation = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    let directory = fixture.0.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let waiting = std::sync::Arc::clone(&barrier);
    let reader = std::thread::spawn(move || {
      waiting.wait();
      let next = acquire_at(&directory, Duration::from_secs(3)).unwrap();
      next.revision().unwrap()
    });
    barrier.wait();
    advance_revision_at(&fixture.0).unwrap();
    let expected = operation.revision().unwrap();
    drop(operation);
    assert_eq!(reader.join().unwrap(), expected);
  }

  #[test]
  fn separate_nonmutating_operations_do_not_revoke_credential_approval() {
    let fixture = Fixture::new();
    let writer = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    advance_revision_at(&fixture.0).unwrap();
    let expected = writer.revision().unwrap();
    drop(writer);
    // Metadata import and discovery share this operation lock, but only an
    // explicit secret mutation advances the durable revocation revision.
    for _ in 0..3 {
      let observer = acquire_at(&fixture.0, Duration::ZERO).unwrap();
      assert_eq!(observer.revision().unwrap(), expected);
      assert_eq!(revision_at(&fixture.0).unwrap(), expected);
    }
  }

  #[test]
  fn unsafe_or_malformed_revision_files_fail_closed_without_mutation() {
    for state in ["symlink", "hardlink", "public", "malformed", "directory"] {
      let fixture = Fixture::new();
      let _operation = acquire_at(&fixture.0, Duration::ZERO).unwrap();
      let revision = fixture.0.join(REVISION_FILE);
      let original = fixture.0.join("original");
      std::fs::write(&original, uuid::Uuid::new_v4().simple().to_string()).unwrap();
      std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600)).unwrap();
      match state {
        "symlink" => symlink(&original, &revision).unwrap(),
        "hardlink" => std::fs::hard_link(&original, &revision).unwrap(),
        "public" => {
          std::fs::copy(&original, &revision).unwrap();
          std::fs::set_permissions(&revision, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        "malformed" => {
          std::fs::write(&revision, "z".repeat(32)).unwrap();
          std::fs::set_permissions(&revision, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        "directory" => std::fs::create_dir(&revision).unwrap(),
        _ => unreachable!(),
      }
      let unchanged = std::fs::read(&original).unwrap();
      assert!(revision_at(&fixture.0).is_err(), "{state}");
      assert!(advance_revision_at(&fixture.0).is_err(), "{state}");
      assert_eq!(std::fs::read(&original).unwrap(), unchanged, "{state}");
    }
  }

  #[test]
  fn revision_checks_reject_nonprivate_or_symlinked_directories() {
    let fixture = Fixture::new();
    let _operation = acquire_at(&fixture.0, Duration::ZERO).unwrap();
    std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(revision_at(&fixture.0).is_err());
    assert!(advance_revision_at(&fixture.0).is_err());
    let linked = Fixture::new();
    symlink(&fixture.0, &linked.0).unwrap();
    assert!(revision_at(&linked.0).is_err());
  }
}
