//! An old driver's pin must fence an entirely new daemon process.
use super::*;
use crate::daemon::{Authority, Delegate, run_daemon};
use ctream_proto::Target;
use std::{
  fs,
  io::{BufRead, Write},
  os::unix::{fs::DirBuilderExt, net::UnixStream},
  path::{Path, PathBuf},
  process::{Child, Command as Process, Stdio},
  thread,
  time::Instant,
};

#[cfg(target_os = "linux")]
mod linux;

struct Fixture(PathBuf);
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}
struct Peer(Child);
impl Drop for Peer {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}
impl Peer {
  fn command(dir: &Path, role: &str) -> io::Result<Process> {
    let mut command = Process::new(std::env::current_exe()?);
    command
      .args([
        "--exact",
        "daemon::server::tests::restart::child",
        "--nocapture",
      ])
      .env("CTREAM_TEST_DAEMON_RUNTIME", dir)
      .env("CTREAM_TEST_TURN_ROLE", role)
      .stdin(Stdio::piped())
      .stdout(Stdio::null());
    Ok(command)
  }
  fn spawn(dir: &Path, role: &str) -> io::Result<Self> {
    Self::command(dir, role)?.spawn().map(Self)
  }
}

#[test]
fn child() -> io::Result<()> {
  match std::env::var("CTREAM_TEST_TURN_ROLE").as_deref() {
    Ok("daemon") => run_daemon(),
    Ok("driver") => {
      let target = Target::MacosDesktop;
      let window = Some("macos-42-9");
      let mut owner = Authority::connect(target, window, "agent", &Uuid::new_v4().to_string())?;
      let token = owner.acquire(30_000)?;
      owner.begin(Some(&token))?;
      let mut driver = Delegate::connect(target, window, owner.delegation_key())?;
      driver.begin()?;
      owner.finish()?;
      drop(owner);
      // The originating controller has gone. Only the driver retains this pin.
      let dir = PathBuf::from(std::env::var_os("CTREAM_TEST_DAEMON_RUNTIME").unwrap());
      fs::write(dir.join("token"), token)?;
      io::copy(&mut io::stdin(), &mut io::sink())?;
      assert!(
        driver.finish().is_err(),
        "old driver must observe its daemon exit"
      );
      Ok(())
    }
    #[cfg(target_os = "linux")]
    Ok(role) => linux::child(role),
    _ => Ok(()),
  }
}

fn until<T>(mut check: impl FnMut() -> Option<T>) -> io::Result<T> {
  let deadline = Instant::now() + IO_TIMEOUT;
  loop {
    if let Some(value) = check() {
      return Ok(value);
    }
    if Instant::now() >= deadline {
      return Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "restart fixture did not become ready",
      ));
    }
    thread::sleep(Duration::from_millis(10));
  }
}

fn send(stream: &mut UnixStream, value: &impl serde::Serialize) -> io::Result<()> {
  serde_json::to_writer(&mut *stream, value)?;
  stream.write_all(b"\n")
}

#[test]
fn new_daemon_waits_for_old_driver_after_crash_and_fences_stale_turn() -> io::Result<()> {
  let dir = Fixture(Path::new("/tmp").join(format!("cturn-{}", Uuid::new_v4().simple())));
  fs::DirBuilder::new().mode(0o700).create(&dir.0)?;
  let path = dir.0.join("v2.sock");
  let old = Peer::spawn(&dir.0, "daemon")?;
  drop(until(|| UnixStream::connect(&path).ok())?);
  let mut driver = Peer::spawn(&dir.0, "driver")?;
  let turn = until(|| {
    fs::read_to_string(dir.0.join("token"))
      .ok()
      .filter(|s| Uuid::try_parse(s).is_ok())
  })?;
  drop(old);
  let _replacement = Peer::spawn(&dir.0, "daemon")?;
  let mut stream = until(|| UnixStream::connect(&path).ok())?;
  let mut read = std::io::BufReader::new(stream.try_clone()?);
  let mut join = request("replacement");
  join.mode = Mode::Authority;
  join.subscribe = false;
  send(&mut stream, &join)?;
  read
    .get_ref()
    .set_read_timeout(Some(Duration::from_millis(150)))?;
  let mut line = String::new();
  let err = read.read_line(&mut line).unwrap_err();
  assert!(matches!(
    err.kind(),
    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
  ));
  drop(driver.0.stdin.take());
  let status = until(|| driver.0.try_wait().ok().flatten())?;
  assert!(status.success());
  read.get_ref().set_read_timeout(Some(IO_TIMEOUT))?;
  read.read_line(&mut line)?;
  assert!(matches!(
    serde_json::from_str::<Reply>(&line)?,
    Reply::Ok { value: Some(_) }
  ));
  send(
    &mut stream,
    &Command::Begin {
      turn_id: Some(turn),
    },
  )?;
  line.clear();
  read.read_line(&mut line)?;
  conflict(serde_json::from_str(&line)?, "stale_turn", None);
  send(&mut stream, &Command::Begin { turn_id: None })?;
  line.clear();
  read.read_line(&mut line)?;
  token(serde_json::from_str(&line)?);
  Ok(())
}
