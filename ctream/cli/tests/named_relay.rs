#![cfg(unix)]

use std::{
  io::{self, Read, Write},
  os::{fd::OwnedFd, unix::net::UnixStream},
  process::{Command, Stdio},
  thread,
  time::{Duration, Instant},
};

#[test]
fn named_session_replies_to_each_socket_request_before_eof() -> io::Result<()> {
  let (mut input, child_input) = UnixStream::pair()?;
  let (mut output, child_output) = UnixStream::pair()?;
  let timeout = Duration::from_secs(5);
  output.set_read_timeout(Some(timeout))?;

  // Node gives subprocesses socket-backed stdio on Unix. The launcher echoes
  // each request through the agent pipe so a stalled request cannot get a reply.
  let mut child = Command::new(env!("CARGO_BIN_EXE_ctream"))
    .args([
      "--host",
      "fixture",
      "--ssh-program",
      "/bin/sh",
      "--ssh-arg",
      "-c",
      "--ssh-arg",
      "exec cat",
      "session",
      ":0",
      "--agent-id",
      "tester",
    ])
    .stdin(Stdio::from(OwnedFd::from(child_input)))
    .stdout(Stdio::from(OwnedFd::from(child_output)))
    .spawn()?;

  let exchange = (|| -> io::Result<()> {
    for request in [b"{\"id\":1}\n", b"{\"id\":2}\n"] {
      input.write_all(request)?;
      let mut reply = vec![0; request.len()];
      output.read_exact(&mut reply)?;
      if reply != request {
        return Err(io::Error::other("unexpected reply from relay"));
      }
    }
    Ok(())
  })();
  drop(input);
  let deadline = Instant::now() + timeout;
  let status = loop {
    if let Some(status) = child.try_wait()? {
      break status;
    }
    if Instant::now() >= deadline {
      child.kill()?;
      child.wait()?;
      return Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "relay did not exit",
      ));
    }
    thread::sleep(Duration::from_millis(10));
  };
  exchange?;
  assert!(status.success(), "relay exited with {status}");
  Ok(())
}
