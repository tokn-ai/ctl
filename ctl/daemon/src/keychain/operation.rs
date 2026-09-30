//! Cross-process serialization for interactive Keychain operations.
//!
//! The lock file contains no credential data. The OS releases the lock if a
//! helper dies, so an import can safely reconcile abandoned metadata writes.

use super::Error;
use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

// Application-local status, deliberately outside the Security OSStatus range.
pub(super) const BUSY: i32 = -1_000_001;
const IO_ERROR: i32 = -36;

pub(super) struct Guard {
  _file: File,
}

pub(super) fn acquire() -> Result<Guard, Error> {
  let directory = dirs::cache_dir()
    .ok_or_else(io_error)?
    .join("io.rmux.desktop.ctld")
    .join("keychain-operations");
  acquire_at(&directory, Duration::from_secs(30))
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
      Ok(()) => return Ok(Guard { _file: file }),
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
  use std::os::unix::fs::symlink;

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
  fn lock_file_symlinks_are_rejected_without_changing_the_target() {
    let fixture = Fixture::new();
    DirBuilder::new().mode(0o700).create(&fixture.0).unwrap();
    let target = fixture.0.join("untouched");
    std::fs::write(&target, b"fixture").unwrap();
    symlink(&target, fixture.0.join("operation.lock")).unwrap();
    assert!(acquire_at(&fixture.0, Duration::ZERO).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"fixture");
  }
}
