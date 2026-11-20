use ctream_driver::{ControlOperation, ControlSource, Result};
use ctream_proto::{Target, Window, valid_window_id};
use std::{
  io,
  process::{Child, Command},
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

impl ControlSource for AtspiWindow {
  fn run(self, operation: ControlOperation) -> Result<()> {
    let operation = match operation {
      ControlOperation::Inspect { bounds: true } => "elements-with-bounds",
      ControlOperation::Inspect { bounds: false } => "elements",
      ControlOperation::Act => "action",
    };
    let mut command = Command::new("python3");
    command.args([
      "-u",
      "-c",
      include_str!("atspi.py"),
      operation,
      &self.window.id,
      &self.window.title,
    ]);
    let mut child = RunningProcess(command.spawn()?);
    let status = child.0.wait()?;
    if !status.success() {
      return Err(io::Error::other(format!("operation backend exited with {status}")).into());
    }
    Ok(())
  }
}
