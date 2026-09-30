//! Persist the endpoint selected by an explicit Connect, without replaying SSH
//! configuration during status polls. Records are hints, never liveness proof.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use sha2::{Digest as _, Sha256};

use super::{MasterEndpoint, RequestError, SharedMasterStartup};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};

const MAX_RECORD_BYTES: u64 = 16 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
  version: u8,
  namespace: String,
  private_path: PathBuf,
  control_path: PathBuf,
  shared: bool,
}

#[derive(Default)]
pub(super) struct Registry {
  directory: Option<PathBuf>,
  namespace: String,
}

impl Registry {
  #[cfg(unix)]
  pub(super) fn for_socket(socket: &Path) -> Self {
    Self {
      directory: Some(super::control_directory().with_file_name("endpoints")),
      namespace: format!(
        "{:x}",
        Sha256::digest(socket.as_os_str().as_encoded_bytes())
      ),
    }
  }

  #[cfg(all(test, unix))]
  pub(super) fn fixture(directory: PathBuf) -> Self {
    Self {
      directory: Some(directory),
      namespace: "fixture-owner".into(),
    }
  }

  pub(super) fn load(&self, private_path: &Path) -> Result<Option<MasterEndpoint>, RequestError> {
    self
      .read(private_path)
      .map_err(|error| registry_error(&error))
  }

  pub(super) fn save(
    &self,
    private_path: &Path,
    endpoint: &MasterEndpoint,
  ) -> Result<(), RequestError> {
    self
      .write(private_path, endpoint)
      .map_err(|error| registry_error(&error))
  }

  pub(super) fn remove(&self, private_path: &Path) -> Result<(), RequestError> {
    self
      .remove_record(private_path)
      .map_err(|error| registry_error(&error))
  }

  fn path(&self, private_path: &Path) -> io::Result<Option<PathBuf>> {
    let Some(directory) = &self.directory else {
      return Ok(None);
    };
    let name = private_path
      .file_name()
      .and_then(|name| name.to_str())
      .ok_or_else(invalid_record)?;
    Ok(Some(
      directory.join(format!("{}-{name}.json", self.namespace)),
    ))
  }

  fn read(&self, private_path: &Path) -> io::Result<Option<MasterEndpoint>> {
    let Some(path) = self.path(private_path)? else {
      return Ok(None);
    };
    if !check_directory(path.parent().ok_or_else(invalid_record)?)? {
      return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(i32::from_ne_bytes(
      (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
        .bits()
        .to_ne_bytes(),
    ));
    let file = match options.open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
      return Err(invalid_record());
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::getuid().as_raw()
      || metadata.mode() & 0o077 != 0
      || metadata.nlink() != 1
    {
      return Err(invalid_record());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
      return Err(invalid_record());
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| invalid_record())?;
    if record.version != 1
      || record.namespace != self.namespace
      || record.private_path != private_path
      || !record.control_path.is_absolute()
      || !record.shared && record.control_path != private_path
    {
      return Err(invalid_record());
    }
    Ok(Some(MasterEndpoint {
      control_path: record.control_path,
      shared: record.shared,
      // Discovery can reuse an external master, never create or own it.
      startup: if record.shared {
        SharedMasterStartup::ExternalOnly
      } else {
        SharedMasterStartup::PrivateFallback
      },
    }))
  }

  fn write(&self, private_path: &Path, endpoint: &MasterEndpoint) -> io::Result<()> {
    let Some(path) = self.path(private_path)? else {
      return Ok(());
    };
    if !endpoint.control_path.is_absolute()
      || !endpoint.shared && endpoint.control_path != private_path
    {
      return Err(invalid_record());
    }
    let record = Record {
      version: 1,
      namespace: self.namespace.clone(),
      private_path: private_path.to_path_buf(),
      control_path: endpoint.control_path.clone(),
      shared: endpoint.shared,
    };
    let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
      return Err(invalid_record());
    }
    let directory = path.parent().ok_or_else(invalid_record)?;
    prepare_directory(directory)?;
    let temporary = super::SocketGuard(directory.join(format!("{}.tmp", uuid::Uuid::new_v4())));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary.0)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary.0, &path)?;
    File::open(directory)?.sync_all()
  }

  fn remove_record(&self, private_path: &Path) -> io::Result<()> {
    let Some(path) = self.path(private_path)? else {
      return Ok(());
    };
    let directory = path.parent().ok_or_else(invalid_record)?;
    if !check_directory(directory)? {
      return Ok(());
    }
    match fs::remove_file(&path) {
      Ok(()) => File::open(directory)?.sync_all(),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
      Err(error) => Err(error),
    }
  }
}

fn check_directory(path: &Path) -> io::Result<bool> {
  let parent = path.parent().ok_or_else(invalid_record)?;
  if !check_private_directory(parent)? {
    return Ok(false);
  }
  check_private_directory(path)
}

fn check_private_directory(path: &Path) -> io::Result<bool> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
    Err(error) => return Err(error),
  };
  if !metadata.is_dir() || metadata.file_type().is_symlink() {
    return Err(invalid_record());
  }
  #[cfg(unix)]
  if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o077 != 0 {
    return Err(invalid_record());
  }
  Ok(true)
}

fn prepare_directory(path: &Path) -> io::Result<()> {
  let parent = path.parent().ok_or_else(invalid_record)?;
  for directory in [parent, path] {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(directory) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
    // Validate each ancestor before creating anything beneath it.
    if !check_private_directory(directory)? {
      return Err(invalid_record());
    }
  }
  check_directory(path).map(|_| ())
}

fn invalid_record() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "invalid or insecure SSH endpoint record",
  )
}

fn registry_error(error: &io::Error) -> RequestError {
  RequestError::MasterObservationFailed(format!("could not access saved SSH endpoint: {error}"))
}

#[cfg(all(test, unix))]
mod tests;
