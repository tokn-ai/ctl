use std::{
  io,
  path::{Path, PathBuf},
  time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt as _};
use zeroize::Zeroizing;

pub const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PASSWORD: usize = 4096;

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

#[must_use]
pub fn invalid(message: &'static str) -> io::Error {
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
/// Reads the credential from a local Unix socket.
///
/// # Errors
/// Returns an error for an unsafe socket path, an I/O failure, a timeout, or invalid input.
pub async fn socket(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
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
pub fn socket(_path: &Path) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  std::future::ready(Err(invalid(
    "--password-socket is Unix-only; use --password-pipe on Windows",
  )))
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

#[cfg(unix)]
/// Reads a password from a claimed descriptor.
///
/// # Errors
/// Returns an error for an I/O failure, a timeout, or invalid input.
pub async fn inherited(file: std::fs::File) -> io::Result<Zeroizing<Vec<u8>>> {
  #[expect(deprecated, reason = "AsyncFd owns a File with a stable descriptor")]
  let descriptor = tokio::io::unix::AsyncFd::new(file)?;
  password(Inherited(descriptor)).await
}

#[cfg(not(unix))]
pub fn inherited(
  file: std::fs::File,
) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  drop(file);
  std::future::ready(Err(invalid(
    "--password-fd is Unix-only; use --password-pipe on Windows",
  )))
}

#[cfg(windows)]
/// Reads the credential from an owner-only Windows named pipe.
///
/// # Errors
/// Returns an error for an invalid pipe name, an I/O failure, a timeout, or invalid input.
pub async fn pipe(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
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
pub fn pipe(_path: &Path) -> impl std::future::Future<Output = io::Result<Zeroizing<Vec<u8>>>> {
  std::future::ready(Err(invalid(
    "--password-pipe is Windows-only; use --password-fd on Unix",
  )))
}

pub struct Listener {
  pub path: PathBuf,
  #[cfg(unix)]
  inner: tokio::net::UnixListener,
  #[cfg(windows)]
  inner: ctmux_ipc::windows::Listener,
}
impl Listener {
  /// Creates a private local askpass listener.
  ///
  /// # Errors
  /// Returns an error if the listener cannot be created.
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
  /// Waits for the local askpass client to connect.
  ///
  /// # Errors
  /// Returns an error if accepting the connection fails.
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
