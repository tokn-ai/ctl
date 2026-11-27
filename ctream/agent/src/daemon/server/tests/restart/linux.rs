//! Simulate a backend which must release input after its Rust parent is killed.
use super::*;

pub(super) fn child(role: &str) -> io::Result<()> {
  let dir = PathBuf::from(std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME").unwrap());
  match role {
    "monitor" => crate::native_input::monitor_linux_helper(),
    "external-owner" => {
      let mut owner =
        Authority::connect(Target::X11(1), None, "owner", &Uuid::new_v4().to_string())?;
      let mut delegate = Delegate::connect(Target::X11(1), None, owner.delegation_key())?;
      let handles = delegate.helper_handles()?;
      let mut command = Peer::command(&dir, "external-driver")?;
      crate::native_input::guard_with(&mut command, handles, Peer::command(&dir, "monitor")?)?;
      let _driver = Peer(command.spawn()?);
      owner.begin(None)?;
      delegate.begin()?;
      fs::write(dir.join("active"), b"")?;
      io::copy(&mut io::stdin(), &mut io::sink())?;
      Ok(())
    }
    "external-driver" => {
      io::copy(&mut io::stdin(), &mut io::sink())?;
      fs::write(dir.join("draining"), b"")?;
      // A bounded fixture guard also catches a broken orphan monitor.
      until(|| dir.join("allow-exit").exists().then_some(()))?;
      fs::write(dir.join("cleaned"), b"")
    }
    _ => Err(io::Error::other("unknown test role")),
  }
}

fn orphan(stuck: bool) -> io::Result<()> {
  let dir = Fixture(Path::new("/tmp").join(format!("cturn-{}", Uuid::new_v4().simple())));
  fs::DirBuilder::new().mode(0o700).create(&dir.0)?;
  let path = dir.0.join("v2.sock");
  let _daemon = Peer::spawn(&dir.0, "daemon")?;
  let mut stream = until(|| UnixStream::connect(&path).ok())?;
  stream.set_read_timeout(Some(IO_TIMEOUT))?;
  let mut replies = io::BufReader::new(stream.try_clone()?);
  let mut join = request("replacement");
  join.mode = Mode::Authority;
  join.subscribe = false;
  send(&mut stream, &join)?;
  let mut line = String::new();
  replies.read_line(&mut line)?;
  token(serde_json::from_str(&line)?);

  let mut actor = Peer::spawn(&dir.0, "external-owner")?;
  until(|| dir.0.join("active").exists().then_some(()))?;
  actor.0.kill()?;
  actor.0.wait()?;
  until(|| dir.0.join("draining").exists().then_some(()))?;
  let mut acquire = || -> io::Result<Reply> {
    send(&mut stream, &Command::Acquire { ttl_ms: 1000 })?;
    line.clear();
    replies.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
  };
  assert!(matches!(acquire()?, Reply::Conflict{ref code,..} if code=="busy"));
  let generation = fs::File::options()
    .read(true)
    .write(true)
    .open(dir.0.join("gestures.lock"))?;
  assert!(matches!(
    generation.try_lock(),
    Err(std::fs::TryLockError::WouldBlock)
  ));
  let start = Instant::now();
  if !stuck {
    fs::write(dir.0.join("allow-exit"), b"")?;
  }
  until(|| matches!(acquire(), Ok(Reply::Ok { value: Some(_) })).then_some(()))?;
  assert_eq!(dir.0.join("cleaned").exists(), !stuck);
  if stuck {
    // The test helper would return on its own after five seconds.
    assert!(start.elapsed() < Duration::from_secs(4));
  }
  until(|| generation.try_lock().ok())?;
  Ok(())
}

#[test]
fn killed_gate_keeps_external_native_cleanup_admitted() -> io::Result<()> {
  orphan(false)
}

#[test]
fn unresponsive_orphan_is_terminated_before_new_admission() -> io::Result<()> {
  orphan(true)
}

#[test]
fn monitor_refuses_registration_from_its_parent_instead_of_a_native_child() -> io::Result<()> {
  use std::io::Read;
  use std::os::fd::OwnedFd;
  let (server, mut peer) = UnixStream::pair()?;
  rustix::net::sockopt::set_socket_passcred(&server, true)?;
  peer.set_read_timeout(Some(IO_TIMEOUT))?;
  let mut command = Peer::command(Path::new("/tmp"), "monitor")?;
  let mut monitor = Peer(
    command
      .stdin(Stdio::from(OwnedFd::from(server)))
      .stderr(Stdio::null())
      .spawn()?,
  );
  drop(command);
  // The parent created this socket and knows the entire handshake. Only the
  // kernel credential of a separate child of that parent can register.
  peer.write_all(b"H")?;
  peer.shutdown(std::net::Shutdown::Write)?;
  let mut response = Vec::new();
  peer.read_to_end(&mut response)?;
  assert!(response.is_empty());
  assert!(!monitor.0.wait()?.success());
  Ok(())
}
