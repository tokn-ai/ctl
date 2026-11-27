//! A native Linux helper may outlive a killed agent. Keep the admission in the
//! actual helper through EOF cleanup, and bound a stuck orphan's lifetime.
use crate::daemon::HelperHandles;
use rustix::{
  event::{PollFd, PollFlags, Timespec, poll},
  io::{Errno, FdFlags, fcntl_setfd},
  net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg, sockopt},
  process::{PidfdFlags, Signal, getppid, pidfd_open, pidfd_send_signal},
};
use std::{
  io::{self, IoSliceMut, Read, Write},
  mem::MaybeUninit,
  os::{
    fd::{AsFd, OwnedFd},
    unix::{net::UnixStream, process::CommandExt},
  },
  process::{Command, Stdio},
  time::{Duration, Instant},
};

const HANDSHAKE: Duration = Duration::from_secs(5);
const ORPHAN_GRACE: Duration = Duration::from_secs(2);

/// Share this gate's socket and crash fence with one native helper before exec.
pub struct LinuxHelperGuard(pub(super) HelperHandles);
impl LinuxHelperGuard {
  pub fn prepare(self, command: &mut Command) -> io::Result<()> {
    let mut monitor = Command::new(std::env::current_exe()?);
    monitor.arg("monitor-native-helper");
    guard_with(command, self.0, monitor)
  }
}

pub(crate) fn guard_with(
  command: &mut Command,
  handles: HelperHandles,
  mut monitor: Command,
) -> io::Result<()> {
  let (server, helper) = UnixStream::pair()?;
  for socket in [&server, &helper] {
    socket.set_read_timeout(Some(HANDSHAKE))?;
    socket.set_write_timeout(Some(HANDSHAKE))?;
  }
  sockopt::set_socket_passcred(&server, true)?;
  let mut monitor = monitor
    .stdin(Stdio::from(OwnedFd::from(server)))
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()?;
  std::thread::Builder::new()
    .name("ctream-helper-monitor".into())
    .spawn(move || {
      let _ = monitor.wait();
    })?;
  // SAFETY: the post-fork closure only uses read, write and fcntl on owned
  // descriptors. No allocator or process-global synchronization is used.
  unsafe {
    command.pre_exec(move || {
      let mut reply = [0];
      if rustix::io::write(&helper, b"H")? != 1
        || rustix::io::read(&helper, &mut reply[..])? != 1
        || reply != *b"M"
      {
        return Err(io::ErrorKind::ConnectionAborted.into());
      }
      fcntl_setfd(&handles.registration, FdFlags::empty())?;
      fcntl_setfd(&handles.fence, FdFlags::empty())?;
      if rustix::io::write(&helper, b"Y")? != 1 {
        return Err(io::ErrorKind::ConnectionAborted.into());
      }
      Ok(())
    });
  }
  Ok(())
}

/// Private child entry point. Stdin must be the inherited Unix socketpair.
/// A PID is never accepted from the wire: Linux authenticates the sending
/// helper, and its live handshake proves it still owns the captured pidfd.
pub fn monitor_linux_helper() -> io::Result<()> {
  let socket = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
  monitor(socket)
}

fn monitor(mut socket: UnixStream) -> io::Result<()> {
  let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
  let origin = sockopt::socket_peercred(&socket)?;
  if getppid() != Some(origin.pid) {
    return Err(denied());
  }
  let parent = pidfd_open(origin.pid, PidfdFlags::empty())?;
  let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmCredentials(1))];
  let mut ancillary = RecvAncillaryBuffer::new(&mut storage);
  let mut byte = [0];
  let data = recvmsg(
    &socket,
    &mut [IoSliceMut::new(&mut byte)],
    &mut ancillary,
    RecvFlags::empty(),
  )?;
  if data.bytes != 1 || byte != *b"H" {
    return Err(denied());
  }
  let sender = ancillary
    .drain()
    .find_map(|msg| match msg {
      RecvAncillaryMessage::ScmCredentials(creds) => Some(creds),
      _ => None,
    })
    .ok_or_else(denied)?;
  if sender.uid != origin.uid {
    return Err(denied());
  }
  let child = pidfd_open(sender.pid, PidfdFlags::empty())?;
  let status = std::fs::read_to_string(format!("/proc/{}/status", sender.pid.as_raw_nonzero()))?;
  let parent_pid = status
    .lines()
    .find_map(|line| line.strip_prefix("PPid:"))
    .and_then(|s| s.trim().parse::<i32>().ok());
  if parent_pid != Some(origin.pid.as_raw_nonzero().get()) {
    return Err(denied());
  }
  socket.write_all(b"M")?;
  socket.read_exact(&mut byte)?;
  if byte != *b"Y" {
    return Err(denied());
  }
  drop(socket);
  let mut fds = [
    PollFd::new(&parent, PollFlags::IN),
    PollFd::new(&child, PollFlags::IN),
  ];
  loop {
    match poll(&mut fds, None) {
      Err(Errno::INTR) => continue,
      result => {
        result?;
        break;
      }
    }
  }
  if !fds[1].revents().is_empty() {
    return Ok(());
  }
  let deadline = Instant::now() + ORPHAN_GRACE;
  loop {
    let mut fds = [PollFd::new(&child, PollFlags::IN)];
    let remaining = Timespec::try_from(deadline.saturating_duration_since(Instant::now())).unwrap();
    match poll(&mut fds, Some(&remaining)) {
      Err(Errno::INTR) => continue,
      Ok(0) => {
        return match pidfd_send_signal(&child, Signal::KILL) {
          Ok(()) | Err(Errno::SRCH) => Ok(()),
          Err(e) => Err(e.into()),
        };
      }
      result => return result.map(|_| ()).map_err(Into::into),
    }
  }
}
