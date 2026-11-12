//! File primitives for per-user installation and deployment storage.

use std::fs::{DirBuilder, File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{
  DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};

/// Whether this is a regular store directory inside the account's namespace.
#[must_use]
pub fn is_directory(metadata: &Metadata) -> bool {
  metadata.is_dir() && is_owned(metadata, 0o022)
}

/// Whether this is a regular file with owner-only write permissions on Unix.
#[must_use]
pub fn is_owned_file(metadata: &Metadata) -> bool {
  metadata.is_file() && is_owned(metadata, 0o022)
}

/// Whether this is a regular file with owner-only access on Unix.
#[must_use]
pub fn is_private_file(metadata: &Metadata) -> bool {
  metadata.is_file() && is_owned(metadata, 0o077)
}

#[cfg(unix)]
fn is_owned(metadata: &Metadata, forbidden_mode: u32) -> bool {
  metadata.uid() == rustix::process::getuid().as_raw() && metadata.mode() & forbidden_mode == 0
}

#[cfg(windows)]
fn is_owned(metadata: &Metadata, _: u32) -> bool {
  // The user's profile supplies inherited ACLs. Reject all reparse points,
  // including junctions, before treating paths as part of that profile.
  const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
  metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
}

/// Defaults to private files and opens the final path component without following links.
#[must_use]
pub fn options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  options.mode(0o600).custom_flags(i32::from_ne_bytes(
    (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC)
      .bits()
      .to_ne_bytes(),
  ));
  #[cfg(windows)]
  {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
  }
  options
}

/// Creates a single private directory, inheriting ACLs on Windows.
///
/// # Errors
/// Reports an existing path, a missing parent, or inaccessible storage.
pub fn create_directory(path: &Path) -> io::Result<()> {
  let mut builder = DirBuilder::new();
  #[cfg(unix)]
  builder.mode(0o700);
  #[cfg(windows)]
  builder.recursive(false);
  builder.create(path)
}

/// Marks a file executable on platforms with executable permission bits.
///
/// # Errors
/// Reports a failure to change permissions.
pub fn set_executable(file: &File) -> io::Result<()> {
  #[cfg(unix)]
  file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
  #[cfg(windows)]
  let _ = file;
  Ok(())
}

/// Whether the file has executable permission bits, when supported by the OS.
#[must_use]
pub fn is_executable(metadata: &Metadata) -> bool {
  #[cfg(unix)]
  return metadata.mode() & 0o111 != 0;
  #[cfg(windows)]
  return is_private_file(metadata);
}

/// Flushes a directory after publishing synced files, where supported.
///
/// # Errors
/// Reports errors opening or syncing a directory on Unix.
pub fn sync_directory(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  File::open(path)?.sync_all()?;
  // Windows does not support flushing a directory through a normal file handle.
  #[cfg(windows)]
  let _ = path;
  Ok(())
}
