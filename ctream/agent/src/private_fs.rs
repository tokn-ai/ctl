//! Private, user-owned directories and files shared by the journal and daemon selection.
use std::{
  fs::{self, DirBuilder, OpenOptions},
  io,
  path::{Path, PathBuf},
};

pub(crate) fn check(metadata: &fs::Metadata, directory: bool, private: bool) -> io::Result<()> {
  let regular = if directory {
    metadata.is_dir()
  } else {
    metadata.is_file()
  };
  #[cfg(unix)]
  let permitted = {
    use std::os::unix::fs::MetadataExt;
    let forbidden = if private { 0o077 } else { 0o022 };
    metadata.uid() == rustix::process::getuid().as_raw()
      && metadata.mode() & forbidden == 0
      && (directory || metadata.nlink() == 1)
  };
  #[cfg(windows)]
  let permitted = {
    use std::os::windows::fs::MetadataExt;
    // LocalAppData supplies the user's inherited ACLs. Reject junctions and
    // other reparse points before following a path within that namespace.
    let _ = private;
    metadata.file_attributes() & 0x400 == 0
  };
  if regular && permitted {
    Ok(())
  } else {
    Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "unsafe desktop state storage",
    ))
  }
}

pub(crate) fn prepare(
  base: &Path,
  components: &[(&str, bool)],
  create: bool,
) -> io::Result<PathBuf> {
  let mut builder = DirBuilder::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
  }
  // The configured OS state root may not exist on the user's first run.
  if create {
    builder.recursive(true).create(base)?;
  }
  check(&fs::symlink_metadata(base)?, true, false)?;
  let mut dir = base.to_owned();
  for &(component, private) in components {
    dir.push(component);
    if create {
      match builder.recursive(false).create(&dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        result => result?,
      }
    }
    check(&fs::symlink_metadata(&dir)?, true, private)?;
  }
  dir.canonicalize()
}

pub(crate) fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    options.mode(0o600).custom_flags(flags.bits() as i32);
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  options
}
