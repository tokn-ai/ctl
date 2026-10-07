//! Owned, cancellable SSH descriptors. Stdout EOF does not require process exit.
use std::io;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Owned, nonblocking input for a disposable SSH protocol process.
pub struct Input(Option<AsyncFd<OwnedFd>>);
/// Owned output that can publish EOF independently of the input descriptor.
pub struct Output(Option<AsyncFd<OwnedFd>>);

/// Called once by a disposable SSH process, before any protocol reads or writes.
/// A duplicate owns stdout; fd 1 remains valid but points to /dev/null, allowing
/// the owned channel to close without violating the standard library's contract.
/// These protocol modes use SSH without a PTY. NONBLOCK changes the inherited
/// pipe or socket's shared file description; this is not a console I/O helper.
///
/// # Errors
/// Returns descriptor duplication, registration, or redirection failures.
pub fn take() -> io::Result<(Input, Output)> {
  let input = Input::new(rustix::stdio::stdin())?;
  let output = descriptor(rustix::stdio::stdout())?;
  let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
  rustix::stdio::dup2_stdout(&null)?;
  Ok((input, Output(Some(output))))
}

impl Input {
  fn new(fd: impl std::os::fd::AsFd) -> io::Result<Self> {
    let metadata = rustix::fs::fstat(&fd)?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode).is_char_device() {
      let null = std::fs::File::open("/dev/null")?;
      if metadata.st_rdev == rustix::fs::fstat(&null)?.st_rdev {
        // Linux epoll cannot register /dev/null, whose input is already EOF.
        return Ok(Self(None));
      }
    }
    descriptor(fd).map(|descriptor| Self(Some(descriptor)))
  }
}

fn descriptor(fd: impl std::os::fd::AsFd) -> io::Result<AsyncFd<OwnedFd>> {
  // Companion daemons must never inherit another writer that keeps SSH alive.
  let owned = rustix::io::fcntl_dupfd_cloexec(fd, 3)?;
  let flags = rustix::fs::fcntl_getfl(&owned)?;
  rustix::fs::fcntl_setfl(&owned, flags | rustix::fs::OFlags::NONBLOCK)?;
  AsyncFd::new(owned)
}

impl AsyncRead for Input {
  fn poll_read(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    if buffer.remaining() == 0 {
      return Poll::Ready(Ok(()));
    }
    let Some(input) = &self.0 else {
      return Poll::Ready(Ok(()));
    };
    loop {
      let mut ready = match input.poll_read_ready(cx) {
        Poll::Ready(Ok(ready)) => ready,
        Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
        Poll::Pending => return Poll::Pending,
      };
      match ready.try_io(|fd| {
        rustix::io::read(fd.get_ref(), buffer.initialize_unfilled()).map_err(io::Error::from)
      }) {
        Ok(Ok(size)) => {
          buffer.advance(size);
          return Poll::Ready(Ok(()));
        }
        Ok(Err(error)) => return Poll::Ready(Err(error)),
        Err(_) => {}
      }
    }
  }
}

impl AsyncWrite for Output {
  fn poll_write(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    let Some(output) = &self.0 else {
      return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
    };
    loop {
      let mut ready = match output.poll_write_ready(cx) {
        Poll::Ready(Ok(ready)) => ready,
        Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
        Poll::Pending => return Poll::Pending,
      };
      if let Ok(result) =
        ready.try_io(|fd| rustix::io::write(fd.get_ref(), bytes).map_err(io::Error::from))
      {
        return Poll::Ready(result);
      }
    }
  }

  fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    if let Some(output) = self.0.take() {
      // OpenSSH can provide a socketpair shared by fd 0 and fd 1. Dropping this
      // duplicate alone leaves its write side open through the input owner.
      match rustix::net::shutdown(output.get_ref(), rustix::net::Shutdown::Write) {
        Ok(()) | Err(rustix::io::Errno::NOTSOCK) => {}
        Err(error) => return Poll::Ready(Err(error.into())),
      }
    }
    Poll::Ready(Ok(()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::process::Stdio;
  use std::time::Duration;
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

  #[tokio::test]
  async fn stdio_child() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    if std::env::var_os("CTL_AGENT_STDIO_TEST").is_none() {
      return;
    }
    let (mut input, mut output) = take().unwrap();
    output.write_all(b"stdio-ready").await.unwrap();
    output.shutdown().await.unwrap();
    let mut late = Vec::new();
    input.read_to_end(&mut late).await.unwrap();
    assert_eq!(late, b"sent-after-output-eof");
  }

  #[tokio::test]
  async fn owned_stdout_delivers_eof_while_stdin_remains_open() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
      .args(["--exact", "stdio::tests::stdio_child", "--nocapture"])
      .env("CTL_AGENT_STDIO_TEST", "1")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), output.read_to_end(&mut bytes))
      .await
      .expect("output EOF cannot depend on closing input")
      .unwrap();
    assert!(bytes.ends_with(b"stdio-ready"));
    assert!(child.try_wait().unwrap().is_none());
    input.write_all(b"sent-after-output-eof").await.unwrap();
    drop(input);
    assert!(
      tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success()
    );
  }

  #[tokio::test]
  async fn socketpair_output_eof_preserves_the_input_direction() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let (local, peer) = std::os::unix::net::UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut peer = tokio::net::UnixStream::from_std(peer).unwrap();
    let mut input = Input::new(&local).unwrap();
    let mut output = Output(Some(descriptor(&local).unwrap()));
    output.write_all(b"response").await.unwrap();
    output.shutdown().await.unwrap();
    output.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), peer.read_to_end(&mut response))
      .await
      .expect("socket EOF cannot depend on dropping the input descriptor")
      .unwrap();
    assert_eq!(response, b"response");
    peer.write_all(b"late-upload").await.unwrap();
    peer.shutdown().await.unwrap();
    let mut upload = Vec::new();
    input.read_to_end(&mut upload).await.unwrap();
    assert_eq!(upload, b"late-upload");
  }

  #[tokio::test]
  async fn an_empty_read_never_waits_for_input() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let (local, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut input = Input::new(local).unwrap();
    tokio::time::timeout(Duration::from_secs(3), input.read(&mut []))
      .await
      .expect("an empty read is ready without input")
      .unwrap();
  }

  #[tokio::test]
  async fn null_input_is_eof_without_registering_a_character_device() {
    let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
    let mut input = Input::new(std::fs::File::open("/dev/null").unwrap()).unwrap();
    assert_eq!(input.read(&mut [0]).await.unwrap(), 0);
  }
}
