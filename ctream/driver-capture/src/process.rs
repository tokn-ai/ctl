mod paced;

use self::paced::LatestFrames;
use ctream_driver::{CaptureLifetime, CaptureSource, Result};
use std::{
  io::{self, Write},
  num::NonZeroUsize,
  process::{Child, Command, Stdio},
  sync::mpsc,
  thread,
  time::Duration,
};

struct RunningProcess(Child);

impl Drop for RunningProcess {
  fn drop(&mut self) {
    let _ = self.0.kill();
    let _ = self.0.wait();
  }
}

/// A streaming, paced fixed-frame, or periodically sampled capture helper.
pub struct ProcessCapture {
  command: Command,
  delivery: Delivery,
}

#[derive(Clone, Copy)]
enum Delivery {
  Stream,
  Sampled(Duration),
  Paced {
    frame_bytes: NonZeroUsize,
    interval: Duration,
  },
}

impl ProcessCapture {
  pub fn stream(command: Command) -> Self {
    Self {
      command,
      delivery: Delivery::Stream,
    }
  }

  pub fn sampled(command: Command, interval: Duration) -> Self {
    Self {
      command,
      delivery: Delivery::Sampled(interval),
    }
  }

  /// Drain fixed-size frames and send the newest one at a bounded rate.
  pub fn paced(command: Command, frame_bytes: NonZeroUsize, interval: Duration) -> Self {
    Self {
      command,
      delivery: Delivery::Paced {
        frame_bytes,
        interval,
      },
    }
  }
}

fn run_child<W: Write + Send>(
  command: &mut Command,
  output: &mut W,
  lifetime: &CaptureLifetime,
  delivery: Delivery,
) -> io::Result<bool> {
  if lifetime.cancelled(Duration::ZERO) {
    return Ok(false);
  }
  let frames = LatestFrames::default();
  let frames = &frames;
  thread::scope(|scope| {
    let mut child = RunningProcess(
      command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?,
    );
    let reader = child.0.stdout.take().expect("piped capture output");
    let (copied, finished) = mpsc::channel();
    match delivery {
      Delivery::Paced {
        frame_bytes,
        interval,
      } => {
        scope.spawn(move || frames.read_from(reader, frame_bytes));
        scope.spawn(move || {
          let _ = copied.send(frames.write_to(output, interval));
        });
      }
      Delivery::Stream | Delivery::Sampled(_) => {
        scope.spawn(move || {
          let mut reader = reader;
          let _ = copied.send(io::copy(&mut reader, output).map(|_| ()));
        });
      }
    }
    let mut transfer_complete = false;
    let completed = loop {
      if !transfer_complete {
        match finished.try_recv() {
          Ok(Ok(())) => transfer_complete = true,
          Ok(Err(error)) => break Err(error),
          Err(mpsc::TryRecvError::Empty) => {}
          Err(mpsc::TryRecvError::Disconnected) => {
            break Err(io::Error::other("capture output worker ended"));
          }
        }
      }
      match child.0.try_wait() {
        Ok(Some(status)) if !status.success() => {
          break Err(io::Error::other(format!(
            "capture backend exited with {status}"
          )));
        }
        Ok(Some(_)) if transfer_complete => break Ok(true),
        Ok(_) => {}
        Err(error) => break Err(error),
      }
      if lifetime.cancelled(Duration::from_millis(20)) {
        break Ok(false);
      }
    };
    if !matches!(completed, Ok(true)) {
      frames.cancel();
    }
    // Closing the child also releases the copy worker's pipe on cancellation.
    drop(child);
    completed
  })
}

impl CaptureSource for ProcessCapture {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    while run_child(&mut self.command, output, lifetime, self.delivery)? {
      let Delivery::Sampled(interval) = self.delivery else {
        break;
      };
      if lifetime.cancelled(interval) {
        break;
      }
    }
    Ok(())
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::time::Instant;

  const PACED: Delivery = Delivery::Paced {
    frame_bytes: NonZeroUsize::new(5).unwrap(),
    interval: Duration::from_millis(125),
  };

  #[test]
  fn cancellation_stops_a_capture_process() {
    for delivery in [Delivery::Stream, PACED] {
      let (notify, closed) = mpsc::channel();
      let lifetime = CaptureLifetime::from_receiver(closed);
      let mut command = Command::new("sleep");
      command.arg("30");
      thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        drop(notify);
      });
      let started = Instant::now();
      assert!(!run_child(&mut command, &mut Vec::new(), &lifetime, delivery).unwrap());
      assert!(started.elapsed() < Duration::from_secs(5));
    }
  }

  #[test]
  fn already_closed_capture_does_not_start_a_helper() {
    let (notify, closed) = mpsc::channel();
    drop(notify);
    let lifetime = CaptureLifetime::from_receiver(closed);
    let mut command = Command::new("missing-ctream-capture-helper");
    assert!(!run_child(&mut command, &mut Vec::new(), &lifetime, Delivery::Stream).unwrap());
  }

  #[test]
  fn helper_frames_are_written_to_the_selected_output() {
    let (_notify, closed) = mpsc::channel();
    let lifetime = CaptureLifetime::from_receiver(closed);
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

    for delivery in [Delivery::Stream, PACED] {
      let (_notify, closed) = mpsc::channel();
      let lifetime = CaptureLifetime::from_receiver(closed);
      let mut command = Command::new("sh");
      command.args(["-c", "printf frame; exec sleep 30"]);
      let started = Instant::now();
      let error = run_child(&mut command, &mut FailedOutput, &lifetime, delivery).unwrap_err();
      assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
      assert!(started.elapsed() < Duration::from_secs(5));
    }
  }
}
