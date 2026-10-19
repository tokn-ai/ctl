use std::{
  io,
  path::{Path, PathBuf},
  time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use zeroize::Zeroizing;

pub(super) const TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const MAX_PASSWORD: usize = 4096;

pub(super) async fn password(reader: impl AsyncRead + Unpin) -> io::Result<Zeroizing<Vec<u8>>> {
  let mut bytes = Zeroizing::new(Vec::new());
  tokio::time::timeout(
    TIMEOUT,
    reader
      .take((MAX_PASSWORD + 1) as u64)
      .read_to_end(&mut bytes),
  )
  .await
  .map_err(|_| invalid("Password channel did not reach EOF within five seconds"))?
  .map_err(|_| invalid("Could not read the password channel"))?;
  if bytes.is_empty()
    || bytes.len() > MAX_PASSWORD
    || bytes.iter().any(|byte| matches!(byte, 0 | b'\n' | b'\r'))
    || std::str::from_utf8(&bytes).is_err()
  {
    return Err(invalid(
      "Password must be 1..4096 UTF-8 bytes without NUL, CR or LF; no trimming is performed",
    ));
  }
  Ok(bytes)
}

pub(super) fn invalid(message: &'static str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(unix)]
struct SocketCleanup {
  path: PathBuf,
  device: u64,
  inode: u64,
}
#[cfg(unix)]
impl Drop for SocketCleanup {
  fn drop(&mut self) {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
      metadata.file_type().is_socket()
        && metadata.dev() == self.device
        && metadata.ino() == self.inode
    }) {
      let _ = std::fs::remove_file(&self.path);
    }
  }
}

#[cfg(unix)]
pub(super) async fn socket(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
  use std::os::unix::fs::MetadataExt as _;
  let parent = path
    .parent()
    .ok_or_else(|| invalid("Password socket needs a private parent directory"))?;
  let metadata = std::fs::symlink_metadata(parent)?;
  if !metadata.is_dir()
    || metadata.file_type().is_symlink()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o077 != 0
  {
    return Err(invalid(
      "Password socket directory must be owned by this user with mode 0700",
    ));
  }
  let listener = tokio::net::UnixListener::bind(path)?;
  let metadata = std::fs::symlink_metadata(path)?;
  let _cleanup = SocketCleanup {
    path: path.into(),
    device: metadata.dev(),
    inode: metadata.ino(),
  };
  let (stream, _) = tokio::time::timeout(TIMEOUT, listener.accept())
    .await
    .map_err(|_| invalid("Password socket connection timed out"))??;
  password(stream).await
}

#[cfg(not(unix))]
pub(super) fn socket(
  _path: &Path,
) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  std::future::ready(Err(invalid(
    "--password-socket is Unix-only; use --password-pipe on Windows",
  )))
}

#[cfg(unix)]
pub(super) fn claim(fd: i32) -> io::Result<std::fs::File> {
  use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
  use std::{os::fd::AsRawFd, os::unix::fs::FileTypeExt as _};
  // This adapter only lends the caller-supplied descriptor to a safe dup API.
  struct Descriptor(i32);
  impl AsRawFd for Descriptor {
    fn as_raw_fd(&self) -> i32 {
      self.0
    }
  }
  if fd < 3 {
    return Err(invalid("Password descriptors must be at least 3"));
  }
  fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
    .map_err(|_| invalid("Password descriptor is not open"))?;
  let duplicate = filedescriptor::FileDescriptor::dup(&Descriptor(fd));
  // Close the inherited descriptor before any SSH/config/helper subprocess.
  nix::unistd::close(fd).map_err(|_| invalid("Could not close the password descriptor"))?;
  let file = duplicate
    .map_err(|_| invalid("Could not duplicate the password descriptor"))?
    .as_file()
    .map_err(|_| invalid("Could not open the password descriptor"))?;
  let kind = file.metadata()?.file_type();
  if !kind.is_fifo() && !kind.is_socket() {
    return Err(invalid(
      "Password descriptor must be a pipe or socket, not a credential file",
    ));
  }
  let flags = fcntl(file.as_raw_fd(), FcntlArg::F_GETFL).map_err(io::Error::from)?;
  fcntl(
    file.as_raw_fd(),
    FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
  )
  .map_err(io::Error::from)?;
  fcntl(file.as_raw_fd(), FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
  Ok(file)
}

#[cfg(unix)]
struct Inherited(tokio::io::unix::AsyncFd<std::fs::File>);
#[cfg(unix)]
impl AsyncRead for Inherited {
  fn poll_read(
    self: std::pin::Pin<&mut Self>,
    context: &mut std::task::Context<'_>,
    buffer: &mut tokio::io::ReadBuf<'_>,
  ) -> std::task::Poll<io::Result<()>> {
    use std::io::Read as _;
    loop {
      let mut ready = std::task::ready!(self.0.poll_read_ready(context))?;
      match ready.try_io(|reader| reader.get_ref().read(buffer.initialize_unfilled())) {
        Ok(Ok(count)) => {
          buffer.advance(count);
          return std::task::Poll::Ready(Ok(()));
        }
        Ok(Err(error)) => return std::task::Poll::Ready(Err(error)),
        Err(_) => {}
      }
    }
  }
}

#[cfg(not(unix))]
pub(super) fn claim(_fd: i32) -> io::Result<std::fs::File> {
  Err(invalid(
    "--password-fd is Unix-only; use --password-pipe on Windows",
  ))
}

#[cfg(unix)]
pub(super) async fn inherited(file: std::fs::File) -> io::Result<Zeroizing<Vec<u8>>> {
  password(Inherited(tokio::io::unix::AsyncFd::new(file)?)).await
}

#[cfg(not(unix))]
pub(super) fn inherited(
  file: std::fs::File,
) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  drop(file);
  std::future::ready(Err(invalid(
    "--password-fd is Unix-only; use --password-pipe on Windows",
  )))
}

#[cfg(windows)]
pub(super) async fn pipe(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
  let name = path
    .to_str()
    .ok_or_else(|| invalid("Invalid local pipe name"))?;
  if !name.starts_with(r"\\.\pipe\ctl-password-")
    || name[9..].contains(['\\', '/'])
    || name.len() > 200
  {
    return Err(invalid(
      r"Use a unique local \\.\pipe\ctl-password-NAME endpoint",
    ));
  }
  // Reuse the exclusive, local-only, owner-DACL pipe implementation. Its handles
  // are non-inheritable; Windows removes the endpoint when they close.
  let listener = ctmux_ipc::windows::Listener::bind(path)?;
  let stream = tokio::time::timeout(TIMEOUT, listener.accept())
    .await
    .map_err(|_| invalid("Password pipe connection timed out"))??
    .0;
  password(stream).await
}

#[cfg(not(windows))]
pub(super) fn pipe(
  _path: &Path,
) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  std::future::ready(Err(invalid(
    "--password-pipe is Windows-only; use --password-fd on Unix",
  )))
}

pub(super) struct Listener {
  pub path: PathBuf,
  #[cfg(unix)]
  inner: tokio::net::UnixListener,
  #[cfg(windows)]
  inner: ctmux_ipc::windows::Listener,
}
impl Listener {
  pub fn new() -> io::Result<Self> {
    let name = uuid::Uuid::new_v4().simple().to_string();
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      let directory = PathBuf::from(format!("/tmp/ctl-auth-{}", &name[..16]));
      std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
      let path = directory.join("askpass.sock");
      match tokio::net::UnixListener::bind(&path) {
        Ok(inner) => Ok(Self { path, inner }),
        Err(error) => {
          let _ = std::fs::remove_dir(directory);
          Err(error)
        }
      }
    }
    #[cfg(windows)]
    {
      let path = PathBuf::from(format!(r"\\.\pipe\ctl-askpass-{name}"));
      let inner = ctmux_ipc::windows::Listener::bind(&path)?;
      Ok(Self { path, inner })
    }
  }
  pub async fn accept(&self) -> io::Result<ctmux_ipc::Stream> {
    self.inner.accept().await.map(|accepted| accepted.0)
  }
}
impl Drop for Listener {
  fn drop(&mut self) {
    #[cfg(unix)]
    {
      let _ = std::fs::remove_file(&self.path);
      if let Some(parent) = self.path.parent() {
        let _ = std::fs::remove_dir(parent);
      }
    }
  }
}
