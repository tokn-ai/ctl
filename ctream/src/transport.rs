use crate::protocol::{Session, Target, VERSION};
use std::{
  io::{self, Read, Write},
  process::{Child, ChildStdout, Command, Stdio},
};

pub enum Request {
  Discover,
  Capture(Target),
  Input(Target),
}

pub struct Channel {
  pub reader: Box<dyn Read + Send>,
  pub writer: Box<dyn Write + Send>,
}

pub trait Transport {
  fn open(&self, request: Request) -> io::Result<Channel>;
}

pub struct Ssh(pub String);

struct SshReader {
  child: Child,
  stdout: ChildStdout,
}

impl Read for SshReader {
  fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
      return Ok(0);
    }
    let count = self.stdout.read(buffer)?;
    if count == 0 {
      let status = self.child.wait()?;
      if !status.success() {
        return Err(io::Error::other(format!("SSH agent exited with {status}")));
      }
    }
    Ok(count)
  }
}

impl Drop for SshReader {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

impl Transport for Ssh {
  fn open(&self, request: Request) -> io::Result<Channel> {
    let args = match request {
      Request::Discover => "discover".to_owned(),
      Request::Capture(target) => format!("capture {target}"),
      Request::Input(target) => format!("input {target}"),
    };
    // Request arguments are fixed words and formatted numeric display names.
    let remote = format!(r#"exec env PATH="$PATH:$HOME/.local/bin" ctream-agent {VERSION} {args}"#);
    let mut child = Command::new("ssh")
      .args([
        "-T",
        "-C",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "--",
        &self.0,
        &remote,
      ])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()?;
    let writer = Box::new(child.stdin.take().expect("piped SSH input"));
    let stdout = child.stdout.take().expect("piped SSH output");
    Ok(Channel {
      reader: Box::new(SshReader { child, stdout }),
      writer,
    })
  }
}

pub fn discover(transport: &dyn Transport) -> io::Result<Vec<Session>> {
  let Channel { reader, writer } = transport.open(Request::Discover)?;
  drop(writer);
  let mut data = Vec::new();
  const LIMIT: usize = 1024 * 1024;
  reader.take((LIMIT + 1) as u64).read_to_end(&mut data)?;
  if data.len() > LIMIT {
    return Err(io::Error::other(
      "ctream agent discovery response exceeds 1 MiB",
    ));
  }
  serde_json::from_slice(&data).map_err(io::Error::other)
}
