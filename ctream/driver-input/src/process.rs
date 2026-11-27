use ctream_driver::{InputSource, Result, UnconfirmedDelivery};
use std::{
  io,
  process::{Child, Command},
};

/// An input helper using inherited stdio and releasing held inputs on EOF.
pub struct InputProcess(pub Command);

impl InputSource for InputProcess {
  fn run_piped(mut self, input: io::PipeReader, output: io::PipeWriter) -> Result<()> {
    self
      .0
      .stdin(input)
      .stdout(output)
      .arg("--sync")
      .env("CTREAM_INPUT_WORKER", "1");
    run_operation(self.0)
  }

  fn run(self) -> Result<()> {
    run_operation(self.0)
  }

  fn run_synchronized(mut self) -> Result<()> {
    self.0.arg("--sync");
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
  let status = child.0.wait().map_err(UnconfirmedDelivery::error)?;
  if !status.success() {
    let error = io::Error::other(format!("operation backend exited with {status}"));
    // Only the helper's synchronized reply identifies a refused command. A
    // nonzero exit can also mean the compositor disappeared during a fence.
    return Err(UnconfirmedDelivery::error(error).into());
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
