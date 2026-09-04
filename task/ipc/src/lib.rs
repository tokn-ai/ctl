//! Per-user transport shared by taskd and its clients.
use std::path::{Path, PathBuf};
use std::{env, io};

#[cfg(windows)]
pub use interprocess::local_socket::tokio::Stream;
#[cfg(windows)]
use interprocess::local_socket::{GenericFilePath, ToFsName, traits::tokio::Stream as _};
#[cfg(unix)]
pub use tokio::net::UnixStream as Stream;

/// Connects to a local task endpoint.
///
/// # Errors
/// Returns the transport error if the endpoint cannot be reached.
pub async fn connect(path: &Path) -> io::Result<Stream> {
  #[cfg(unix)]
  {
    Stream::connect(path).await
  }
  #[cfg(windows)]
  {
    Stream::connect(path.to_fs_name::<GenericFilePath>()?).await
  }
}

/// The default per-user task endpoint. On Windows this is a local named pipe.
///
/// # Panics
/// On Windows, panics if neither a local user data directory nor
/// `TASKD_RUNTIME_DIR` is available.
#[must_use]
pub fn socket_path() -> PathBuf {
  #[cfg(unix)]
  {
    if let Some(directory) = env::var_os("TASKD_RUNTIME_DIR") {
      return PathBuf::from(directory).join("taskd.sock");
    }
    if let Some(directory) = env::var_os("XDG_RUNTIME_DIR") {
      return PathBuf::from(directory).join("taskd/taskd.sock");
    }
    let uid = rustix::process::getuid().as_raw();
    PathBuf::from("/tmp").join(format!("taskd-{uid}/taskd.sock"))
  }
  #[cfg(windows)]
  {
    use std::os::windows::ffi::OsStrExt;
    // A stable namespace, not an authentication secret; the pipe ACL controls access.
    let directory = env::var_os("TASKD_RUNTIME_DIR").map_or_else(
      || dirs::data_local_dir().expect("Windows user has a local data directory"),
      PathBuf::from,
    );
    let bytes: Vec<u8> = directory
      .as_os_str()
      .encode_wide()
      .flat_map(u16::to_le_bytes)
      .collect();
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &bytes);
    PathBuf::from(format!(r"\\.\pipe\ctl-taskd-{id}"))
  }
}

#[cfg(windows)]
pub mod windows {
  use super::{GenericFilePath, Path, ToFsName, io};
  use interprocess::local_socket::{ListenerOptions, tokio::Listener};
  use interprocess::os::windows::{
    local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
  };
  use widestring::u16cstr;

  /// Binds a local-only pipe with access restricted to its owner.
  ///
  /// # Errors
  /// Returns an error if the ACL or exclusive first pipe instance cannot be created.
  pub fn bind(path: &Path) -> io::Result<Listener> {
    let descriptor = SecurityDescriptor::deserialize(u16cstr!("D:P(A;;GA;;;OW)"))?;
    ListenerOptions::new()
      .name(path.to_fs_name::<GenericFilePath>()?)
      .security_descriptor(descriptor)
      .create_tokio()
  }
}
