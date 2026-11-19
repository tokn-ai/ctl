use super::{ControlOperation, ControlSource, Result, process::run_operation};
use crate::protocol::Window;
use std::process::Command;

/// An accessible window resolved through the selected compositor session.
pub struct AtspiWindow {
  window: Window,
}

impl AtspiWindow {
  pub(super) fn new(window: Window) -> Self {
    Self { window }
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
      include_str!("../atspi.py"),
      operation,
      &self.window.id,
      &self.window.title,
    ]);
    run_operation(command)
  }
}
