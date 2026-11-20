use ctream_driver::{InputSource, Result};
use std::{
  io,
  process::{Child, Command},
};

/// An input helper using inherited stdio and releasing held inputs on EOF.
pub struct InputProcess(pub Command);

impl InputSource for InputProcess {
  fn run(self) -> Result<()> {
    run_operation(self.0)
  }
}

struct RunningProcess(Child);

impl Drop for RunningProcess {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

fn run_operation(mut command: Command) -> Result<()> {
  let mut child = RunningProcess(command.spawn()?);
  let status = child.0.wait()?;
  if !status.success() {
    return Err(io::Error::other(format!("operation backend exited with {status}")).into());
  }
  Ok(())
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[test]
  fn input_helpers_return_to_the_owner_and_report_failure() {
    InputProcess(Command::new("true")).run().unwrap();
    let result = InputProcess(Command::new("false")).run();
    assert!(result.is_err());
  }
}
