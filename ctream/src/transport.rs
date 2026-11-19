use crate::protocol::{
  Action, ActionResult, DISPLAY_VERSION, Element, Session, Target, VERSION, WAYLAND_WINDOW_VERSION,
  Window, X11_WINDOW_VERSION, valid_window_id,
};
use serde::de::DeserializeOwned;
use std::{
  io::{self, Read, Write},
  process::{Child, ChildStdout, Command, Stdio},
};

pub enum Request {
  Discover,
  Capture(Target),
  Input(Target),
  Windows(Target),
  CaptureWindow(Target, String),
  Elements(Target, String),
  ElementsWithBounds(Target, String),
  Action(Target, String),
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
    let command = match request {
      Request::Discover => format!("{DISPLAY_VERSION} discover"),
      Request::Capture(target) => format!("{DISPLAY_VERSION} capture {target}"),
      Request::Input(target) => format!("{DISPLAY_VERSION} input {target}"),
      Request::Windows(target) => format!("{} windows {target}", window_version(target)),
      Request::CaptureWindow(target, id) => window_command("capture-window", target, &id)?,
      Request::Elements(target, id) => window_command("elements", target, &id)?,
      Request::ElementsWithBounds(target, id) => {
        window_command_at(VERSION, "elements", target, &id)?
      }
      Request::Action(target, id) => window_command("action", target, &id)?,
    };
    let remote = format!(r#"exec env PATH="$PATH:$HOME/.local/bin" ctream-agent {command}"#);
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

fn window_version(target: Target) -> &'static str {
  match target {
    Target::X11(_) => X11_WINDOW_VERSION,
    Target::Wayland(_) => WAYLAND_WINDOW_VERSION,
  }
}

fn window_command(operation: &str, target: Target, id: &str) -> io::Result<String> {
  window_command_at(window_version(target), operation, target, id)
}

fn window_command_at(
  version: &str,
  operation: &str,
  target: Target,
  id: &str,
) -> io::Result<String> {
  if !valid_window_id(id) {
    return Err(io::Error::other("invalid window identifier"));
  }
  Ok(format!("{version} {operation} {target} {id}"))
}

fn json_response<T: DeserializeOwned>(channel: Channel) -> io::Result<T> {
  let Channel { reader, writer } = channel;
  drop(writer);
  let mut data = Vec::new();
  const LIMIT: usize = 1024 * 1024;
  reader.take((LIMIT + 1) as u64).read_to_end(&mut data)?;
  if data.len() > LIMIT {
    return Err(io::Error::other("ctream agent response exceeds 1 MiB"));
  }
  serde_json::from_slice(&data).map_err(io::Error::other)
}

pub fn discover(transport: &dyn Transport) -> io::Result<Vec<Session>> {
  json_response(transport.open(Request::Discover)?)
}

pub fn windows(transport: &dyn Transport, target: Target) -> io::Result<Vec<Window>> {
  json_response(transport.open(Request::Windows(target))?)
}

pub fn elements(transport: &dyn Transport, target: Target, id: String) -> io::Result<Vec<Element>> {
  let inspection = transport
    .open(Request::ElementsWithBounds(target, id.clone()))
    .and_then(json_response);
  inspection.or_else(|error| {
    transport
      .open(Request::Elements(target, id))
      .and_then(json_response)
      .map_err(|_| error)
  })
}

pub fn action(
  transport: &dyn Transport,
  target: Target,
  id: String,
  action: Action,
) -> io::Result<ActionResult> {
  let data = serde_json::to_vec(&action)?;
  if data.len() > 65536 {
    return Err(io::Error::other("action exceeds the request limit"));
  }
  let mut channel = transport.open(Request::Action(target, id))?;
  channel.writer.write_all(&data)?;
  json_response(channel)
}
