use super::{CaptureSource, InputSource, Result};
use std::{
  io::{self, Read, Write},
  process::{Child, Command, Stdio},
  sync::mpsc::{self, Receiver, RecvTimeoutError},
  thread,
  time::Duration,
};

/// EOF on the operation's input cancels its capture resource.
pub struct CaptureLifetime {
  closed: Receiver<()>,
}

impl CaptureLifetime {
  pub fn from_reader(mut input: impl Read + Send + 'static) -> Self {
    let (notify, closed) = mpsc::channel();
    thread::spawn(move || {
      let _ = io::copy(&mut input, &mut io::sink());
      let _ = notify.send(());
    });
    Self { closed }
  }

  pub fn cancelled(&self, delay: Duration) -> bool {
    !matches!(
      self.closed.recv_timeout(delay),
      Err(RecvTimeoutError::Timeout)
    )
  }
}

/// The existing input helpers own stdin and release held inputs when it closes.
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

pub(super) fn run_operation(mut command: Command) -> Result<()> {
  let mut child = RunningProcess(command.spawn()?);
  let status = child.0.wait()?;
  if !status.success() {
    return Err(io::Error::other(format!("operation backend exited with {status}")).into());
  }
  Ok(())
}

/// A streaming helper, or a still-image helper sampled at a fixed interval.
pub struct ProcessCapture {
  command: Command,
  repeat: Option<Duration>,
}

impl ProcessCapture {
  pub fn stream(command: Command) -> Self {
    Self {
      command,
      repeat: None,
    }
  }

  pub fn sampled(command: Command, interval: Duration) -> Self {
    Self {
      command,
      repeat: Some(interval),
    }
  }
}

fn run_child<W: Write + Send>(
  command: &mut Command,
  output: &mut W,
  lifetime: &CaptureLifetime,
) -> io::Result<bool> {
  if lifetime.cancelled(Duration::ZERO) {
    return Ok(false);
  }
  thread::scope(|scope| {
    let mut child = RunningProcess(
      command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?,
    );
    let mut reader = child.0.stdout.take().expect("piped capture output");
    let (copied, finished) = mpsc::channel();
    scope.spawn(move || {
      let _ = copied.send(io::copy(&mut reader, output));
    });
    let mut transfer_complete = false;
    let completed = loop {
      if let Ok(result) = finished.try_recv() {
        result?;
        transfer_complete = true;
      }
      if let Some(status) = child.0.try_wait()? {
        if !status.success() {
          return Err(io::Error::other(format!(
            "capture backend exited with {status}"
          )));
        }
        break true;
      }
      if lifetime.cancelled(Duration::from_millis(20)) {
        break false;
      }
    };
    // Closing the child also releases the copy worker's pipe on cancellation.
    drop(child);
    if completed && !transfer_complete {
      finished
        .recv()
        .map_err(|_| io::Error::other("capture output worker ended"))??;
    }
    Ok(completed)
  })
}

impl CaptureSource for ProcessCapture {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    while run_child(&mut self.command, output, lifetime)? {
      let Some(interval) = self.repeat else { break };
      if lifetime.cancelled(interval) {
        break;
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Instant;

  #[test]
  fn cancellation_stops_a_capture_process() {
    let (notify, closed) = mpsc::channel();
    let lifetime = CaptureLifetime { closed };
    let mut command = Command::new("sleep");
    command.arg("30");
    thread::spawn(move || {
      thread::sleep(Duration::from_millis(40));
      drop(notify);
    });
    let started = Instant::now();
    assert!(!run_child(&mut command, &mut Vec::new(), &lifetime).unwrap());
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[test]
  fn already_closed_capture_does_not_start_a_helper() {
    let (notify, closed) = mpsc::channel();
    drop(notify);
    let lifetime = CaptureLifetime { closed };
    let mut command = Command::new("missing-ctream-capture-helper");
    assert!(!run_child(&mut command, &mut Vec::new(), &lifetime).unwrap());
  }

  #[test]
  fn helper_frames_are_written_to_the_selected_output() {
    let (_notify, closed) = mpsc::channel();
    let lifetime = CaptureLifetime { closed };
    let mut command = Command::new("printf");
    command.arg("P6\n1 1\n255\nabc");
    let mut output = Vec::new();
    ProcessCapture::stream(command)
      .run(&mut output, &lifetime)
      .unwrap();
    assert_eq!(output, b"P6\n1 1\n255\nabc");
  }

  #[test]
  fn output_failure_stops_a_running_helper() {
    struct FailedOutput;
    impl Write for FailedOutput {
      fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
      }

      fn flush(&mut self) -> io::Result<()> {
        Ok(())
      }
    }

    let (_notify, closed) = mpsc::channel();
    let lifetime = CaptureLifetime { closed };
    let mut command = Command::new("sh");
    command.args(["-c", "printf frame; exec sleep 30"]);
    let started = Instant::now();
    let error = run_child(&mut command, &mut FailedOutput, &lifetime).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    assert!(started.elapsed() < Duration::from_secs(5));
  }

  #[test]
  fn input_helpers_return_to_the_owner_and_report_failure() {
    InputProcess(Command::new("true")).run().unwrap();
    let result = InputProcess(Command::new("false")).run();
    assert!(result.is_err());
  }
}
