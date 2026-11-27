use interprocess::local_socket::{tokio::Stream, traits::StreamCommon};
use rustix::{fs::OFlags, process::geteuid};
use std::{
  fs::{self, File, OpenOptions},
  io,
  os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
  path::{Path, PathBuf},
};

pub(super) fn path() -> io::Result<PathBuf> {
  // SSH and GUI sessions can have different environment-specific temp roots.
  let base = Path::new("/tmp").join(format!("ctream-live-{}", geteuid().as_raw()));
  #[cfg(any(test, all(debug_assertions, feature = "test-fixtures")))]
  let base = std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME")
    .map(PathBuf::from)
    .unwrap_or(base);
  match fs::DirBuilder::new().mode(0o700).create(&base) {
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
    result => result?,
  }
  let metadata = fs::symlink_metadata(&base)?;
  if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o777 != 0o700
  {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "unsafe desktop daemon directory",
    ));
  }
  Ok(base.join("v2.sock"))
}

pub(super) fn acquire(path: &Path) -> io::Result<File> {
  let lock = private_file(&path.with_extension("lock"))?;
  lock.try_lock()?;
  match fs::symlink_metadata(path) {
    Ok(info) if info.file_type().is_socket() => fs::remove_file(path)?,
    Ok(_) => return Err(io::Error::other("desktop daemon socket path is occupied")),
    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  Ok(lock)
}

pub(in crate::daemon) fn fence_file() -> io::Result<File> {
  private_file(&path()?.with_file_name("gestures.lock"))
}

fn private_file(path: &Path) -> io::Result<File> {
  let flags = OFlags::NOFOLLOW | OFlags::NONBLOCK;
  let lock = OpenOptions::new()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .custom_flags(flags.bits() as i32)
    .mode(0o600)
    .open(path)?;
  let info = lock.metadata()?;
  if !info.is_file()
    || info.uid() != geteuid().as_raw()
    || info.nlink() != 1
    || info.mode() & 0o077 != 0
  {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "unsafe desktop daemon lock",
    ));
  }
  Ok(lock)
}

pub(in crate::daemon) fn check_client(stream: &Stream) -> io::Result<()> {
  if stream.peer_creds()?.euid() == Some(geteuid().as_raw()) {
    Ok(())
  } else {
    Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "live peer has a different account",
    ))
  }
}

pub(super) use check_client as check_server;
