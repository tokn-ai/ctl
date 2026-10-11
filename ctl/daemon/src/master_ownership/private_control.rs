//! The hello/terminate subset of OpenSSH's documented multiplexing protocol.
//! <https://github.com/openssh/openssh-portable/blob/master/PROTOCOL.mux>

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};

use crate::{MASTER_CHECK_TIMEOUT, MasterEndpoint, socket_identity};

const HELLO: u32 = 0x0000_0001;
const VERSION: u32 = 4;
const TERMINATE: u32 = 0x1000_0005;
const OK: u32 = 0x8000_0001;
const REQUEST_ID: u32 = 0;
const MAX_PACKET_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct Control {
  path: PathBuf,
  identity: Option<(u64, u64)>,
  socket: Option<UnixStream>,
}

impl Control {
  pub(super) fn new(endpoint: &MasterEndpoint) -> Self {
    Self {
      path: endpoint.control_path.clone(),
      ..Self::default()
    }
  }

  pub(super) async fn pin(&mut self) -> io::Result<()> {
    if self.socket.is_some() {
      return Ok(());
    }
    let identity = socket_identity(&self.path).ok_or_else(missing_socket)?;
    self.identity = Some(identity);
    let socket = tokio::time::timeout(
      MASTER_CHECK_TIMEOUT,
      tokio::net::UnixStream::connect(&self.path),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SSH mux connection timed out"))??;
    if socket_identity(&self.path) != Some(identity) {
      return Err(missing_socket());
    }
    self.socket = Some(socket.into_std()?);
    Ok(())
  }

  fn pin_for_cleanup(&mut self) -> io::Result<()> {
    if self.socket.is_some() {
      return Ok(());
    }
    let identity = socket_identity(&self.path).ok_or_else(missing_socket)?;
    if self.identity.is_some_and(|previous| previous != identity) {
      return Err(missing_socket());
    }
    // After killing/reaping the foreground child, any forked master has
    // already published its socket. The retained target lock excludes a new
    // ctld attempt; an observed replacement is never adopted for cleanup.
    self.identity = Some(identity);
    let socket = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
    rustix::fs::fcntl_setfl(&socket, rustix::fs::OFlags::NONBLOCK)?;
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
    rustix::net::connect(&socket, &SocketAddrUnix::new(&self.path)?)?;
    if socket_identity(&self.path) != Some(identity) {
      return Err(missing_socket());
    }
    self.socket = Some(UnixStream::from(socket));
    Ok(())
  }

  pub(super) fn terminate(&mut self) -> io::Result<()> {
    if let Err(error) = self.pin_for_cleanup() {
      if error.kind() == io::ErrorKind::NotFound || !self.path.exists() {
        return Ok(());
      }
      return Err(error);
    }
    let Some(mut socket) = self.socket.take() else {
      return Ok(());
    };
    // A pinned connection addresses this master's socket object, even if a
    // replacement has since appeared at the original pathname.
    terminate(&mut socket)
  }
}

fn missing_socket() -> io::Error {
  io::Error::new(
    io::ErrorKind::NotFound,
    "private SSH master changed or disappeared",
  )
}

pub(super) fn terminate(socket: &mut UnixStream) -> io::Result<()> {
  socket
    .set_nonblocking(true)
    .map_err(|error| io_error("nonblocking mode", &error))?;
  let deadline = Instant::now() + MASTER_CHECK_TIMEOUT;
  write_packet(socket, &[HELLO, VERSION], deadline)?;
  let hello = read_packet(socket, deadline)?;
  if hello.len() < 8 || hello[..8] != words(&[HELLO, VERSION]) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "unsupported SSH mux version",
    ));
  }
  write_packet(socket, &[TERMINATE, REQUEST_ID], deadline)?;
  read_termination_response(socket, deadline)
}

pub(super) fn read_termination_response(
  socket: &mut UnixStream,
  deadline: Instant,
) -> io::Result<()> {
  let response = match read_packet(socket, deadline) {
    Ok(response) => response,
    // OpenSSH's terminate handler may exit before its queued OK is flushed.
    // Its own mux client accepts this EOF after the complete request is sent.
    Err(error)
      if matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
      ) =>
    {
      return Ok(());
    }
    Err(error) => return Err(error),
  };
  if response != words(&[OK, REQUEST_ID]) {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "SSH mux termination refused",
    ));
  }
  Ok(())
}

fn words(values: &[u32]) -> Vec<u8> {
  values
    .iter()
    .flat_map(|value| value.to_be_bytes())
    .collect()
}

fn remaining(deadline: Instant) -> io::Result<Timespec> {
  let remaining = deadline.saturating_duration_since(Instant::now());
  if remaining.is_zero() {
    return Err(io::Error::new(
      io::ErrorKind::TimedOut,
      "SSH mux cleanup timed out",
    ));
  }
  Ok(Timespec::try_from(remaining).expect("bounded SSH mux timeout"))
}

fn wait_for_io(socket: &UnixStream, events: PollFlags, deadline: Instant) -> io::Result<()> {
  loop {
    let timeout = remaining(deadline)?;
    let mut descriptor = [PollFd::new(socket, events)];
    match rustix::event::poll(&mut descriptor, Some(&timeout)) {
      Ok(0) | Err(rustix::io::Errno::INTR) => {}
      // Read/write reports the actual EOF or error, including buffered data
      // followed by HUP. No socket options are changed after peer shutdown.
      Ok(_) => return Ok(()),
      Err(error) => return Err(io_error("poll", &error.into())),
    }
  }
}

fn io_error(stage: &str, error: &io::Error) -> io::Error {
  io::Error::new(error.kind(), format!("SSH mux {stage}: {error}"))
}

fn write_packet(socket: &mut UnixStream, values: &[u32], deadline: Instant) -> io::Result<()> {
  let packet = words(values);
  write_all(
    socket,
    &u32::try_from(packet.len()).unwrap().to_be_bytes(),
    deadline,
  )?;
  write_all(socket, &packet, deadline)
}

fn read_packet(socket: &mut UnixStream, deadline: Instant) -> io::Result<Vec<u8>> {
  let mut length = [0_u8; 4];
  read_exact(socket, &mut length, deadline)?;
  let length = u32::from_be_bytes(length) as usize;
  if !(8..=MAX_PACKET_BYTES).contains(&length) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid SSH mux packet length",
    ));
  }
  let mut packet = vec![0; length];
  read_exact(socket, &mut packet, deadline)?;
  Ok(packet)
}

fn write_all(socket: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
  while !bytes.is_empty() {
    remaining(deadline)?;
    match socket.write(bytes) {
      Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
      Ok(count) => bytes = &bytes[count..],
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        wait_for_io(socket, PollFlags::OUT, deadline)?;
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
      Err(error) => return Err(io_error("write", &error)),
    }
  }
  Ok(())
}

fn read_exact(socket: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
  while !bytes.is_empty() {
    remaining(deadline)?;
    match socket.read(bytes) {
      Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
      Ok(count) => bytes = &mut bytes[count..],
      Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
        wait_for_io(socket, PollFlags::IN, deadline)?;
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
      Err(error) => return Err(io_error("read", &error)),
    }
  }
  Ok(())
}
