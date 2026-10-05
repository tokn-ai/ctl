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
  verification_app: Option<PathBuf>,
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
      verification_app: None,
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
        let existing = read_manifest(&session.destination, &session.manifest.target)?;
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

  #[cfg(target_os = "macos")]
  pub fn open_installed(
    home: &Path,
    target: &str,
    installation: &ctl_ipc::managed::CompatibleInstallation,
  ) -> Result<Self, Error> {
    let manifest = read_manifest(&installation.directory, target)?;
    if manifest.development.is_some() != installation.development {
      return Err(Error::InvalidRelease(
        "selected helper signing policy does not match its cache".into(),
      ));
    }
    let root = ctl_ipc::managed::component_directory(home);
    let expected = if installation.development {
      root.join("development").join(&manifest.sha256)
    } else {
      root.join("versions").join(manifest.directory_name())
    };
    if expected.canonicalize()? != installation.directory.canonicalize()? {
      return Err(Error::InvalidRelease(
        "selected helper location does not match its installation manifest".into(),
      ));
    }
    let session = Self::begin(home, manifest)?;
    if !session.reused {
      return Err(Error::InvalidRelease(
        "selected helper changed while opening its installation".into(),
      ));
    }
    Ok(session)
  }

  pub fn work(&self) -> &Path {
    &self.work
  }

  #[cfg(target_os = "macos")]
  pub fn verification(home: &Path, directory: &Path, manifest: Manifest) -> Result<Self, Error> {
    let root = ctl_ipc::managed::ensure_component_directory(home)?;
    let lock = SetupLock::acquire(&root)?;
    let work = root.join(format!(".setup-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&work)?;
    Ok(Self {
      home: home.to_owned(),
      root,
      work,
      destination: directory.to_owned(),
      manifest,
      reused: true,
      _lock: lock,
      verification_app: Some(directory.join("ctld.app")),
    })
  }

  fn payload(&self) -> PathBuf {
    self.work.join("payload")
  }

  #[cfg(target_os = "macos")]
  pub fn app(&self) -> PathBuf {
    if let Some(app) = &self.verification_app {
      return app.clone();
    }
    if self.reused {
      self.destination.join("ctld.app")
    } else {
      self.payload().join("ctld.app")
    }
  }

  pub fn executable(&self) -> Result<PathBuf, Error> {
    if let Some(app) = &self.verification_app {
      let executable = app.join("Contents/MacOS/ctld");
      if !fs::symlink_metadata(&executable)?.is_file() {
        return Err(Error::Verification(
          "helper executable is not a regular file".into(),
        ));
      }
      return Ok(executable);
    }
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
    if self.verification_app.is_some() {
      return Err(Error::Verification(
        "a passive package verification cannot activate a helper".into(),
      ));
    }
    if self.manifest.development.is_none() {
      ctl_ipc::managed::validate_current_selection(&self.home)?;
    }
    ctl_ipc::managed::validate_compatible_selection(&self.home, &self.manifest.target)?;
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
      self.select_compatible()?;
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
    self.select_compatible()?;
    Ok(self.outcome(executable))
  }

  fn select_compatible(&self) -> Result<(), Error> {
    ctl_ipc::managed::select_compatible_installation(
      &self.home,
      &self.manifest.target,
      &self.destination,
      self.manifest.development.is_some(),
    )?;
    Ok(())
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

fn read_manifest(directory: &Path, target: &str) -> Result<Manifest, Error> {
  let marker = File::from(
    rustix::fs::open(
      directory.join("installation.json"),
      rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC,
      rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?,
  );
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
  Manifest::parse_installed(&contents, target)
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
