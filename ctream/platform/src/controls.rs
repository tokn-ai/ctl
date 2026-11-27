use ctream_driver::{ControlOperation, ControlSource, Result, UnconfirmedDelivery};
use ctream_proto::{Target, Window, valid_window_id};
use std::{
  io::{self, Read, Write},
  process::{Child, Command, Stdio},
};

/// An accessible window resolved through the selected compositor session.
pub struct AtspiWindow {
  window: Window,
}

pub fn resolve(target: Target, id: &str) -> Result<AtspiWindow> {
  if !matches!(target, Target::Wayland(_)) {
    return Err("semantic actions currently require a supported Wayland compositor".into());
  }
  if !valid_window_id(id) {
    return Err("invalid window identifier".into());
  }
  let window = crate::windows::list(target)?
    .into_iter()
    .find(|window| window.id == id)
    .ok_or("selected window is no longer available")?;
  Ok(AtspiWindow { window })
}

struct RunningProcess(Child);

impl Drop for RunningProcess {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

impl AtspiWindow {
  fn command(&self, operation: &str) -> Command {
    let mut command = Command::new("python3");
    command.args([
      "-u",
      "-c",
      include_str!("atspi.py"),
      operation,
      &self.window.id,
      &self.window.title,
    ]);
    command
  }

  /// The host has already read the complete bounded request before admitting
  /// this mutation. An idle or incomplete client cannot own the desktop.
  pub fn apply(
    &self,
    request: &[u8],
    prepare: impl FnOnce(&mut Command) -> io::Result<()>,
  ) -> io::Result<Vec<u8>> {
    let mut command = self.command("action");
    prepare(&mut command)?;
    let mut child = RunningProcess(
      command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?,
    );
    let mut stdin = child.0.stdin.take().unwrap();
    stdin.write_all(request)?;
    drop(stdin);
    let mut reply = Vec::new();
    child
      .0
      .stdout
      .take()
      .unwrap()
      .take(65537)
      .read_to_end(&mut reply)
      .map_err(UnconfirmedDelivery::error)?;
    // Drop the reader above before waiting, including when the reply exceeded
    // the bound and the helper was still writing.
    if !child
      .0
      .wait()
      .map_err(UnconfirmedDelivery::error)?
      .success()
      || reply.len() > 65536
      || reply.is_empty()
    {
      return Err(UnconfirmedDelivery::error(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "semantic driver returned no complete outcome",
      )));
    }
    Ok(reply)
  }
}

impl ControlSource for AtspiWindow {
  fn run(self, operation: ControlOperation) -> Result<()> {
    let operation = match operation {
      ControlOperation::Inspect { bounds: true } => "elements-with-bounds",
      ControlOperation::Inspect { bounds: false } => "elements",
      ControlOperation::Act => "action",
    };
    let mut child = RunningProcess(self.command(operation).spawn()?);
    let status = child.0.wait()?;
    if !status.success() {
      return Err(io::Error::other(format!("operation backend exited with {status}")).into());
    }
    Ok(())
  }
}
