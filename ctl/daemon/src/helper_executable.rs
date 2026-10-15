//! Self-executed SSH helpers must remain available for the running broker.

use std::io;
use std::path::{Path, PathBuf};

pub(super) fn current() -> io::Result<PathBuf> {
  let executable = std::env::current_exe().map_err(|error| {
    io::Error::new(
      error.kind(),
      format!(
        "The running ctld helper could not be located: {error}. Restart ctld with `ctl components restart ctld` before opening a new SSH connection."
      ),
    )
  })?;
  validate(&executable)?;
  Ok(executable)
}

pub(super) fn validate(executable: &Path) -> io::Result<()> {
  let unavailable = |error: io::Error| {
    io::Error::new(
      error.kind(),
      format!(
        "The running ctld helper is no longer available at {}: {error}. Restart ctld with `ctl components restart ctld` before opening a new SSH connection.",
        executable.display()
      ),
    )
  };
  let metadata = std::fs::metadata(executable).map_err(unavailable)?;
  if !metadata.is_file() {
    return Err(unavailable(io::Error::new(
      io::ErrorKind::InvalidInput,
      "the helper path is not a file",
    )));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o111 == 0 {
      return Err(unavailable(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "the helper file is not executable",
      )));
    }
  }
  Ok(())
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::os::unix::fs::PermissionsExt as _;

  #[test]
  fn helpers_require_an_existing_executable_file_and_report_recovery() {
    let directory = std::env::temp_dir().join(format!(
      "ctld-helper-check-{}",
      uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&directory).unwrap();
    let executable = directory.join("ctld");
    for (path, kind) in [
      (executable.as_path(), io::ErrorKind::NotFound),
      (directory.as_path(), io::ErrorKind::InvalidInput),
    ] {
      let error = validate(path).unwrap_err();
      assert_eq!(error.kind(), kind);
      assert!(error.to_string().contains("ctl components restart ctld"));
      assert!(error.to_string().contains(&*path.to_string_lossy()));
    }
    std::fs::write(&executable, b"synthetic nonexecuted helper").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
      validate(&executable).unwrap_err().kind(),
      io::ErrorKind::PermissionDenied
    );
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    validate(&executable).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
  }
}
