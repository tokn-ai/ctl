#![cfg(any(target_os = "macos", target_os = "linux"))]

use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use process_info::{Foreground, Inspector};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Cleanup(Box<dyn Child + Send + Sync>);
impl Drop for Cleanup {
  fn drop(&mut self) {
    if !matches!(self.0.try_wait(), Ok(Some(_))) {
      let _ = self.0.kill();
      let _ = self.0.wait();
    }
  }
}

#[test]
fn observes_a_real_shell_cwd_foreground_job_and_return_to_shell_without_hooks() {
  let pair = native_pty_system().openpty(PtySize::default()).unwrap();
  let mut command = CommandBuilder::new("/bin/sh");
  command.arg("-i");
  command.env("ENV", "/dev/null");
  command.cwd("/tmp");
  let mut child = Cleanup(pair.slave.spawn_command(command).unwrap());
  let inspector = Inspector::new(child.0.process_id().unwrap()).unwrap();
  drop(pair.slave);
  let mut writer = pair.master.take_writer().unwrap();
  let mut reader = pair.master.try_clone_reader().unwrap();
  let (send, receive) = mpsc::channel();
  std::thread::spawn(move || {
    let mut buf = [0; 1024];
    while let Ok(n) = reader.read(&mut buf) {
      if n == 0 || send.send(buf[..n].to_vec()).is_err() {
        break;
      }
    }
  });
  // Octal spelling prevents the command's PTY echo from looking like its output.
  writer
    .write_all(b"cd /; printf '\\137\\137ready\\137\\137\\n'\n")
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(10);
  let mut output = Vec::new();
  while !output.windows(9).any(|window| window == b"__ready__") {
    output.extend(
      receive
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap(),
    );
  }
  let group = || {
    pair
      .master
      .process_group_leader()
      .and_then(|pid| u32::try_from(pid).ok())
  };
  let snapshot = inspector.inspect(group()).unwrap();
  assert_eq!(snapshot.cwd.as_deref(), Some(std::path::Path::new("/")));
  assert_eq!(snapshot.foreground, Foreground::Shell);

  writer.write_all(b"sleep 60\n").unwrap();
  wait_until(|| {
    matches!(inspector.inspect(group()).unwrap().foreground,
    Foreground::Child(ref process) if process.name.as_deref() == Some("sleep"))
  });
  writer.write_all(&[3]).unwrap();
  wait_until(|| inspector.inspect(group()).unwrap().foreground == Foreground::Shell);
  writer.write_all(b"exit\n").unwrap();
  child.0.wait().unwrap();
  assert!(inspector.inspect(group()).is_err());
}

fn wait_until(mut condition: impl FnMut() -> bool) {
  let deadline = Instant::now() + Duration::from_secs(5);
  while !condition() {
    assert!(
      Instant::now() < deadline,
      "process observation did not converge"
    );
    std::thread::sleep(Duration::from_millis(20));
  }
}
