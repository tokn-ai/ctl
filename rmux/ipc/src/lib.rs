use std::env;
use std::io;
use std::path::{Path, PathBuf};

const RUNTIME_DIRECTORY_ENV: &str = "RMUX_RUNTIME_DIR";

#[must_use]
pub fn socket_path() -> PathBuf {
  runtime_directory().join("rmux.sock")
}

#[must_use]
pub fn runtime_directory() -> PathBuf {
  if let Some(directory) = env::var_os(RUNTIME_DIRECTORY_ENV) {
    return PathBuf::from(directory);
  }

  if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
    return PathBuf::from(directory).join("rmux");
  }

  fallback_runtime_directory()
}

/// Creates and validates the private directory containing a local endpoint.
///
/// # Errors
///
/// Returns an error when the endpoint has no parent, the directory cannot be
/// created, or the directory is not private and owned by the current user.
pub fn prepare_runtime_directory(path: &Path) -> io::Result<()> {
  let directory = path.parent().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      format!("socket path {} has no parent directory", path.display()),
    )
  })?;
  let existed = directory.exists();
  std::fs::create_dir_all(directory)?;
  if !existed {
    set_owner_only_permissions(directory)?;
  }
  secure_runtime_directory(directory)
}

#[cfg(unix)]
fn fallback_runtime_directory() -> PathBuf {
  let uid = rustix::process::getuid().as_raw();
  PathBuf::from("/tmp").join(format!("rmux-{uid}"))
}

#[cfg(not(unix))]
fn fallback_runtime_directory() -> PathBuf {
  env::temp_dir().join("rmux")
}

#[cfg(unix)]
fn secure_runtime_directory(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};

  let metadata = std::fs::symlink_metadata(directory)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime path {} is not a real directory",
        directory.display()
      ),
    ));
  }

  let expected_uid = rustix::process::getuid().as_raw();
  if metadata.uid() != expected_uid {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is owned by another user",
        directory.display()
      ),
    ));
  }

  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "runtime directory {} is accessible by other users",
        directory.display()
      ),
    ));
  }

  Ok(())
}

#[cfg(not(unix))]
fn secure_runtime_directory(_directory: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(directory: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt;

  std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_directory: &Path) -> io::Result<()> {
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn socket_is_inside_runtime_directory() {
    assert_eq!(socket_path().file_name().unwrap(), "rmux.sock");
  }
}
