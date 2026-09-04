//! Owner-restricted local Windows named pipes.
use interprocess::local_socket::{
  GenericFilePath, ListenerOptions, ToFsName, tokio, traits::tokio::Listener as _,
};
use interprocess::os::windows::{
  local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
};
use std::{
  io,
  os::windows::ffi::OsStrExt,
  path::{Path, PathBuf},
};
use widestring::u16cstr;

pub(crate) fn endpoint(directory: &Path) -> PathBuf {
  let bytes: Vec<u8> = directory
    .as_os_str()
    .encode_wide()
    .flat_map(u16::to_le_bytes)
    .collect();
  let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &bytes);
  PathBuf::from(format!(r"\\.\pipe\ctl-rmuxd-{id}"))
}

/// A local-only listener. Windows removes its endpoint when handles close.
pub struct Listener(tokio::Listener);
impl Listener {
  /// Creates an exclusive first instance with an owner-only DACL.
  ///
  /// # Errors
  /// Returns a name, ACL, or pipe creation error.
  pub fn bind(path: &Path) -> io::Result<Self> {
    let descriptor = SecurityDescriptor::deserialize(u16cstr!("D:P(A;;GA;;;OW)"))?;
    ListenerOptions::new()
      .name(path.to_fs_name::<GenericFilePath>()?)
      .security_descriptor(descriptor)
      .create_tokio()
      .map(Self)
  }
  /// Accepts a local client.
  ///
  /// # Errors
  /// Returns the underlying pipe accept error.
  pub async fn accept(&self) -> io::Result<(super::Stream, ())> {
    self.0.accept().await.map(|stream| (stream, ()))
  }
}
