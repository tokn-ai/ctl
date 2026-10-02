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
  _lock: File,
}

impl Session {
  pub fn begin(home: &Path, manifest: Manifest) -> Result<Self, Error> {
    let root = ctl_ipc::managed::ensure_component_directory(home)?;
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
    // A well-formed dangling selection can be repaired by verified setup, while
    // unsafe paths and foreign-owned selections are never silently replaced.
    ctl_ipc::managed::validate_current_selection(home)?;
    let work = root.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    let mut session = Self {
      home: home.to_owned(),
      destination: root.join("versions").join(manifest.directory_name()),
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
        let existing = Manifest::parse(
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
    Ok(ctl_ipc::managed::resolve_candidate_executable(
      &self.home, &candidate,
    )?)
  }

  pub fn unpack(self, bytes: &[u8]) -> Result<Self, Error> {
    archive::extract(bytes, &self.manifest, &self.payload())?;
    Ok(self)
  }

  // Called only after platform signature checks and the bounded metadata query.
  pub fn activate(self) -> Result<SetupOutcome, Error> {
    ctl_ipc::managed::validate_current_selection(&self.home)?;
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
    let executable = ctl_ipc::managed::resolve_candidate_executable(&self.home, &self.destination)?;
    let next = self.root.join(format!(".current-{}", uuid::Uuid::new_v4()));
    symlink(
      Path::new("versions").join(self.manifest.directory_name()),
      &next,
    )?;
    if let Err(error) = fs::rename(&next, self.root.join("current")) {
      let _ = fs::remove_file(&next);
      return Err(error.into());
    }
    Ok(SetupOutcome {
      component: "ctld",
      version: self.manifest.app_version.clone(),
      executable,
      reused: self.reused,
    })
  }
}

impl Drop for Session {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.work);
  }
}
