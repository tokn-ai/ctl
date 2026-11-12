use rustix::fs::{CWD, Mode, OFlags, RenameFlags, renameat_with};
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

pub(super) struct Workspace {
  destination: PathBuf,
  storage: PathBuf,
  staging: PathBuf,
  lock: File,
  keep_staging: bool,
}

impl Workspace {
  pub(super) fn begin(destination: &Path) -> io::Result<Self> {
    let name = destination.file_name().ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "app destination needs a filename",
      )
    })?;
    let parent = destination
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    let destination = parent.join(name);
    let mut storage_name = OsString::from(".ctlup-");
    storage_name.push(name);
    let storage = parent.join(storage_name);
    let mut directories = fs::DirBuilder::new();
    directories.mode(0o700);
    match directories.create(&storage) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(&storage)?;
    if !metadata.is_dir() || !private(&metadata) {
      return Err(io::Error::other(format!(
        "app staging directory must be private and owned by this user: {}",
        storage.display()
      )));
    }
    let lock = lock(&storage)?;
    let staging = storage.join(format!("stage-{}", uuid::Uuid::new_v4()));
    directories.create(&staging)?;
    Ok(Self {
      destination,
      storage,
      staging,
      lock,
      keep_staging: false,
    })
  }

  pub(super) fn staging(&self) -> &Path {
    &self.staging
  }

  /// Publish a verified payload. A fresh install claims an unused path. An
  /// update swaps the previously inspected app and staged app in one operation.
  pub(super) fn activate(
    &mut self,
    staged: &Path,
    expected: Option<&Metadata>,
  ) -> io::Result<Option<PathBuf>> {
    if staged.parent() != Some(self.staging()) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "app payload is outside its staging directory",
      ));
    }
    let payload = fs::symlink_metadata(staged)?;
    if !(payload.is_file() || payload.is_dir()) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "app payload must be a regular file or directory",
      ));
    }
    let Some(expected) = expected else {
      rename(staged, &self.destination, RenameFlags::NOREPLACE)?;
      return Ok(None);
    };
    let existing = existing(&self.destination)?.ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "the installed app disappeared during the update",
      )
    })?;
    if existing.dev() != expected.dev()
      || existing.ino() != expected.ino()
      || existing.ctime() != expected.ctime()
      || existing.ctime_nsec() != expected.ctime_nsec()
    {
      return Err(io::Error::other(
        "the installed app changed during the update",
      ));
    }
    if !((existing.is_file() && payload.is_file()) || (existing.is_dir() && payload.is_dir())) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "the installed app type differs from the staged app",
      ));
    }

    rename(staged, &self.destination, RenameFlags::EXCHANGE)?;
    // Activation is complete. If retaining the previous app at its usual path
    // fails, leave it at its exchanged path so it remains available to recover.
    let previous = self.storage.join("previous");
    let saved = rename(staged, &previous, RenameFlags::NOREPLACE).or_else(|error| {
      if error.kind() == io::ErrorKind::AlreadyExists {
        rename(staged, &previous, RenameFlags::EXCHANGE)
      } else {
        Err(error)
      }
    });
    if saved.is_err() {
      self.keep_staging = true;
      return Ok(Some(staged.to_owned()));
    }
    Ok(Some(previous))
  }
}

impl Drop for Workspace {
  fn drop(&mut self) {
    if !self.keep_staging {
      let _ = fs::remove_dir_all(&self.staging);
    }
    let _ = self.lock.unlock();
  }
}

pub(super) fn existing(destination: &Path) -> io::Result<Option<Metadata>> {
  match fs::symlink_metadata(destination) {
    Ok(metadata) => Ok(Some(metadata)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}

#[expect(clippy::verbose_bit_mask, reason = "octal Unix permission mask")]
fn private(metadata: &Metadata) -> bool {
  metadata.uid() == rustix::process::getuid().as_raw() && metadata.mode() & 0o077 == 0
}

fn lock(storage: &Path) -> io::Result<File> {
  let file = File::from(rustix::fs::open(
    storage.join("install.lock"),
    OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
    Mode::from_raw_mode(0o600),
  )?);
  let metadata = file.metadata()?;
  if !metadata.is_file() || !private(&metadata) {
    return Err(io::Error::other(
      "app install lock must be a private owned file",
    ));
  }
  file.try_lock().map_err(|error| match error {
    fs::TryLockError::WouldBlock => io::Error::new(
      io::ErrorKind::WouldBlock,
      "another ctlup app install or update is in progress at this destination",
    ),
    fs::TryLockError::Error(error) => error,
  })?;
  Ok(file)
}

fn rename(source: &Path, destination: &Path, flags: RenameFlags) -> io::Result<()> {
  renameat_with(CWD, source, CWD, destination, flags).map_err(io::Error::from)
}

#[cfg(test)]
mod tests;
