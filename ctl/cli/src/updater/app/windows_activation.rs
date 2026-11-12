//! Windows directory replacement with a retained rollback and restart recovery.
//!
//! Windows cannot exchange nonempty directories. The fixed pending directory
//! records the interrupted rename: restore it if the destination is absent,
//! otherwise activation completed and it becomes the retained previous app.

use ctl_core::private_fs;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

pub(super) struct Workspace {
  destination: PathBuf,
  storage: PathBuf,
  staging: PathBuf,
  lock: File,
}

impl Workspace {
  pub(super) fn begin(destination: &Path) -> io::Result<Self> {
    let parent = destination
      .parent()
      .ok_or_else(|| invalid("app destination needs a parent"))?;
    require_directory(parent)?;
    let storage = parent.join(".ctlup-ctmux");
    match private_fs::create_directory(&storage) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
    require_directory(&storage)?;
    let lock = private_fs::options()
      .read(true)
      .write(true)
      .create(true)
      .truncate(false)
      .open(storage.join("install.lock"))?;
    if !private_fs::is_private_file(&lock.metadata()?) {
      return Err(invalid("app install lock must be a regular file"));
    }
    lock.try_lock().map_err(|error| match error {
      fs::TryLockError::WouldBlock => io::Error::new(
        io::ErrorKind::WouldBlock,
        "another ctlup app install or update is in progress",
      ),
      fs::TryLockError::Error(error) => error,
    })?;
    let mut workspace = Self {
      destination: destination.to_owned(),
      staging: storage.join(format!("stage-{}", uuid::Uuid::new_v4())),
      storage,
      lock,
    };
    workspace.recover()?;
    private_fs::create_directory(&workspace.staging)?;
    Ok(workspace)
  }

  pub(super) fn staging(&self) -> &Path {
    &self.staging
  }

  pub(super) fn activate(
    &mut self,
    staged: &Path,
    expected: Option<&Metadata>,
  ) -> io::Result<Option<PathBuf>> {
    if staged.parent() != Some(self.staging()) {
      return Err(invalid("app payload is outside its staging directory"));
    }
    require_directory(staged)?;
    let Some(expected) = expected else {
      if existing(&self.destination)?.is_some() {
        return Err(io::Error::new(
          io::ErrorKind::AlreadyExists,
          "app appeared during installation",
        ));
      }
      // Windows rename never replaces a directory that appeared concurrently.
      fs::rename(staged, &self.destination)?;
      return Ok(None);
    };
    let actual = existing(&self.destination)?
      .ok_or_else(|| invalid("installed app disappeared during update"))?;
    if actual.creation_time() != expected.creation_time()
      || actual.last_write_time() != expected.last_write_time()
    {
      return Err(invalid("installed app changed during update"));
    }
    let _leases = close_app_leases(&self.destination)?;
    let pending = self.storage.join("pending-previous");
    fs::rename(&self.destination, &pending)?;
    if let Err(error) = fs::rename(staged, &self.destination) {
      return match fs::rename(&pending, &self.destination) {
        Ok(()) => Err(error),
        Err(restore) => Err(io::Error::other(format!(
          "app activation failed: {error}; rollback is retained at {} but restoration failed: {restore}. Run ctlup app install again to recover.",
          pending.display()
        ))),
      };
    }
    // Keep the successfully activated app even if retention cleanup is blocked.
    Ok(Some(self.retain_previous().unwrap_or(pending)))
  }

  fn recover(&mut self) -> io::Result<()> {
    let pending = self.storage.join("pending-previous");
    if existing(&pending)?.is_none() {
      return Ok(());
    }
    if existing(&self.destination)?.is_none() {
      fs::rename(pending, &self.destination)?;
    } else {
      self.retain_previous()?;
    }
    Ok(())
  }

  fn retain_previous(&self) -> io::Result<PathBuf> {
    let previous = self.storage.join("previous");
    let retired = self
      .storage
      .join(format!("retired-{}", uuid::Uuid::new_v4()));
    let previous_leases = if existing(&previous)?.is_some() {
      // Refuse cleanup before touching any older retained tree. remove_dir_all
      // can otherwise delete resources before failing on a running executable.
      Some(close_app_leases(&previous)?)
    } else {
      None
    };
    let moved = if previous_leases.is_some() {
      fs::rename(&previous, &retired)?;
      true
    } else {
      false
    };
    let pending = self.storage.join("pending-previous");
    if let Err(error) = fs::rename(&pending, &previous) {
      if moved {
        let _ = fs::rename(&retired, &previous);
      }
      return Err(error);
    }
    if moved {
      // Its old public path has moved and all files were leased through the
      // rename. Release our own handles before removing the retired directory.
      drop(previous_leases);
      let _ = fs::remove_dir_all(retired);
    }
    Ok(previous)
  }
}

impl Drop for Workspace {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.staging);
    let _ = self.lock.unlock();
  }
}

pub(super) fn existing(path: &Path) -> io::Result<Option<Metadata>> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if private_fs::is_directory(&metadata) => Ok(Some(metadata)),
    Ok(_) => Err(invalid(
      "portable app paths must be directories without reparse points",
    )),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

fn require_directory(path: &Path) -> io::Result<()> {
  existing(path)?
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "app directory is missing"))?;
  Ok(())
}

fn close_app_leases(root: &Path) -> io::Result<Vec<File>> {
  let mut leases = Vec::new();
  for entry in fs::read_dir(root)? {
    let path = entry?.path();
    let metadata = fs::symlink_metadata(&path)?;
    if private_fs::is_directory(&metadata) {
      leases.extend(close_app_leases(&path)?);
    } else if private_fs::is_owned_file(&metadata) {
      // A mapped executable denies write access. Deny new readers/writers while
      // retaining FILE_SHARE_DELETE so the directory can be renamed safely.
      // Hold these handles until replacement completes to prevent relaunches.
      let file = private_fs::options()
        .write(true)
        .share_mode(0x4)
        .open(path)
        .map_err(|error| {
          io::Error::new(
            error.kind(),
            format!(
              "Close the ctmux desktop app and its local helpers, then retry the update: {error}"
            ),
          )
        })?;
      if !private_fs::is_owned_file(&file.metadata()?) {
        return Err(invalid("installed app file changed into an unsafe file"));
      }
      leases.push(file);
    } else {
      return Err(invalid("installed app contains a special file"));
    }
  }
  Ok(leases)
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "windows_activation/tests.rs"]
mod tests;
