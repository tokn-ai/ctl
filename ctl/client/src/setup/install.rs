use super::{Error, SetupOutcome, archive, manifest::Manifest};
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{
  DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink,
};
use std::path::{Path, PathBuf};

pub(super) struct Session {
  home: PathBuf,
  root: PathBuf,
  work: PathBuf,
  destination: PathBuf,
  pub manifest: Manifest,
  pub reused: bool,
  _lock: SetupLock,
}

struct SetupLock(File);

impl SetupLock {
  fn acquire(root: &Path) -> Result<Self, Error> {
    let lock = rustix::fs::open(
      root.join("setup.lock"),
      rustix::fs::OFlags::CREATE
        | rustix::fs::OFlags::RDWR
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::from_raw_mode(0o600),
    )
    .map_err(std::io::Error::from)?;
    let lock = File::from(lock);
    let metadata = lock.metadata()?;
    if !metadata.is_file()
      || metadata.uid() != rustix::process::getuid().as_raw()
      || metadata.permissions().mode() & 0o077 != 0
    {
      return Err(Error::InvalidRelease(
        "setup lock is not a private owned file".into(),
      ));
    }
    lock.try_lock().map_err(|error| match error {
      fs::TryLockError::WouldBlock => Error::Busy,
      fs::TryLockError::Error(error) => Error::Io(error),
    })?;
    Ok(Self(lock))
  }
}

impl Drop for SetupLock {
  fn drop(&mut self) {
    // Closing our descriptor alone can leave a forked child's descriptor
    // holding the lock until exec. Explicitly release the shared lock now.
    let _ = self.0.unlock();
  }
}

impl Session {
  pub fn begin(home: &Path, manifest: Manifest) -> Result<Self, Error> {
    let root = ctl_ipc::managed::ensure_component_directory(home)?;
    let lock = SetupLock::acquire(&root)?;
    // A well-formed dangling selection can be repaired by verified setup, while
    // unsafe paths and foreign-owned selections are never silently replaced.
    let destination = if manifest.development.is_some() {
      ctl_ipc::managed::ensure_development_directory(home)?.join(&manifest.sha256)
    } else {
      ctl_ipc::managed::validate_current_selection(home)?;
      root.join("versions").join(manifest.directory_name())
    };
    let work = root.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    let mut session = Self {
      home: home.to_owned(),
      destination,
      root,
      work,
      manifest,
      reused: false,
      _lock: lock,
    };
    fs::DirBuilder::new()
      .mode(0o700)
      .create(session.payload())?;
    match fs::symlink_metadata(&session.destination) {
      Ok(metadata) => {
        if !metadata.is_dir()
          || metadata.uid() != rustix::process::getuid().as_raw()
          || metadata.permissions().mode() & 0o077 != 0
        {
          return Err(Error::InvalidRelease(
            "existing version is not a private owned directory".into(),
          ));
        }
        let marker = rustix::fs::open(
          session.destination.join("installation.json"),
          rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
          rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?;
        let marker = File::from(marker);
        let metadata = marker.metadata()?;
        if !metadata.is_file()
          || metadata.uid() != rustix::process::getuid().as_raw()
          || metadata.permissions().mode() & 0o077 != 0
        {
          return Err(Error::InvalidRelease(
            "existing installation marker is not a private owned regular file".into(),
          ));
        }
        let mut contents = Vec::new();
        marker
          .take(super::manifest::MAX_MANIFEST_BYTES as u64 + 1)
          .read_to_end(&mut contents)?;
        let parse = if session.manifest.development.is_some() {
          Manifest::parse_development
        } else {
          Manifest::parse
        };
        let existing = parse(
          &contents,
          &session.manifest.app_version,
          &session.manifest.target,
        )?;
        if existing != session.manifest {
          return Err(Error::InvalidRelease("this version is already installed with different release contents; it was left unchanged".into()));
        }
        session.reused = true;
      }
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(error) => return Err(error.into()),
    }
    Ok(session)
  }

  pub fn work(&self) -> &Path {
    &self.work
  }

  fn payload(&self) -> PathBuf {
    self.work.join("payload")
  }

  #[cfg(target_os = "macos")]
  pub fn app(&self) -> PathBuf {
    if self.reused {
      self.destination.join("ctld.app")
    } else {
      self.payload().join("ctld.app")
    }
  }

  pub fn executable(&self) -> Result<PathBuf, Error> {
    let candidate = if self.reused {
      self.destination.clone()
    } else {
      self.payload()
    };
    let resolve = if self.manifest.development.is_some() {
      ctl_ipc::managed::resolve_development_candidate_executable
    } else {
      ctl_ipc::managed::resolve_candidate_executable
    };
    Ok(resolve(&self.home, &candidate)?)
  }

  pub fn unpack(self, bytes: &[u8]) -> Result<Self, Error> {
    archive::extract(bytes, &self.manifest, &self.payload())?;
    Ok(self)
  }

  // Called only after platform signature checks and the bounded metadata query.
  pub fn activate(self) -> Result<SetupOutcome, Error> {
    if self.manifest.development.is_none() {
      ctl_ipc::managed::validate_current_selection(&self.home)?;
    }
    self.executable()?;
    if !self.reused {
      let marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(self.payload().join("installation.json"))?;
      serde_json::to_writer(&marker, &self.manifest).map_err(std::io::Error::other)?;
      marker.sync_all()?;
      fs::rename(self.payload(), &self.destination)?;
    }
    let executable = self.executable_after_activation()?;
    if self.manifest.development.is_some() {
      return Ok(self.outcome(executable));
    }
    let next = self.root.join(format!(".current-{}", uuid::Uuid::new_v4()));
    symlink(
      Path::new("versions").join(self.manifest.directory_name()),
      &next,
    )?;
    if let Err(error) = fs::rename(&next, self.root.join("current")) {
      let _ = fs::remove_file(&next);
      return Err(error.into());
    }
    Ok(self.outcome(executable))
  }

  fn executable_after_activation(&self) -> Result<PathBuf, Error> {
    let resolve = if self.manifest.development.is_some() {
      ctl_ipc::managed::resolve_development_candidate_executable
    } else {
      ctl_ipc::managed::resolve_candidate_executable
    };
    Ok(resolve(&self.home, &self.destination)?)
  }

  fn outcome(&self, executable: PathBuf) -> SetupOutcome {
    SetupOutcome {
      component: "ctld",
      version: self.manifest.app_version.clone(),
      executable,
      reused: self.reused,
    }
  }
}

impl Drop for Session {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.work);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Home(PathBuf);

  impl Home {
    fn new() -> Self {
      let path =
        std::env::temp_dir().join(format!("ctld-setup-lock-test-{}", uuid::Uuid::new_v4()));
      fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for Home {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  #[expect(
    clippy::used_underscore_binding,
    reason = "the regression duplicates the descriptor held by the otherwise unread RAII guard"
  )]
  fn dropping_session_releases_lock_while_an_inherited_descriptor_remains_open() {
    let home = Home::new();
    let manifest = super::super::manifest::fixture();
    let session = Session::begin(&home.0, manifest.clone()).unwrap();
    // A duplicated descriptor shares the same open file description, as an
    // unrelated subprocess does between fork and exec despite CLOEXEC.
    let inherited = session._lock.0.try_clone().unwrap();
    let staging = session.work().to_owned();
    drop(session);
    assert!(!staging.exists());
    let next = Session::begin(&home.0, manifest).unwrap();
    drop(inherited);
    drop(next);
  }
}
