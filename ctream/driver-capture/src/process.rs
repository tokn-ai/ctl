mod paced;

use self::paced::LatestFrames;
use ctream_driver::{CaptureLifetime, CaptureSource, Result};
use std::{
  io::{self, Write},
  num::NonZeroUsize,
  process::{Child, Command, Stdio},
  sync::mpsc,
  thread,
  time::{Duration, Instant},
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
  SampledAtRate(Duration),
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

  /// Schedule each snapshot from its start, including helper execution time.
  pub fn sampled_at_rate(command: Command, interval: Duration) -> Self {
    Self {
      command,
      delivery: Delivery::SampledAtRate(interval),
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
      Delivery::Stream | Delivery::Sampled(_) | Delivery::SampledAtRate(_) => {
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
      let poll_interval = if matches!(delivery, Delivery::SampledAtRate(_)) {
        Duration::from_millis(2)
      } else {
        Duration::from_millis(20)
      };
      if lifetime.cancelled(poll_interval) {
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

// An event-driven capture helper may never write again if its window is idle.
// Release its native resources even if the capture daemon is killed abruptly.
#[cfg(target_os = "linux")]
fn stop_with_worker(command: &mut Command) {
  use rustix::process::{self, Signal};
  use std::os::unix::process::CommandExt;
  let parent = process::getpid();
  // SAFETY: this pre-exec hook only issues prctl/getppid and constructs an
  // errno on failure. No locks, allocations or other post-fork runtime work.
  unsafe {
    command.pre_exec(move || {
      process::set_parent_process_death_signal(Some(Signal::KILL))?;
      if process::getppid() != Some(parent) {
        return Err(io::Error::from_raw_os_error(
          rustix::io::Errno::PIPE.raw_os_error(),
        ));
      }
      Ok(())
    });
  }
}

impl CaptureSource for ProcessCapture {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    #[cfg(target_os = "linux")]
    stop_with_worker(&mut self.command);
    let mut next_capture = Instant::now();
    loop {
      if !run_child(&mut self.command, output, lifetime, self.delivery)? {
        break;
      }
      let wait = match self.delivery {
        Delivery::Sampled(interval) => interval,
        Delivery::SampledAtRate(interval) => {
          // Snapshots are fresh work, so missed capture periods are skipped.
          // Retain the phase across ordinary scheduler wake-up delays.
          next_capture = (next_capture + interval).max(Instant::now());
          next_capture.saturating_duration_since(Instant::now())
        }
        _ => break,
      };
      if lifetime.cancelled(wait) {
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

  #[cfg(target_os = "linux")]
  #[test]
  fn an_idle_native_helper_exits_when_its_owning_worker_dies() {
    let mut child = RunningProcess(
      thread::spawn(|| {
        let mut shell = Command::new("sleep");
        shell.arg("30");
        stop_with_worker(&mut shell);
        shell.spawn().unwrap()
      })
      .join()
      .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
      if let Some(status) = child.0.try_wait().unwrap() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
          status.signal(),
          Some(rustix::process::Signal::KILL.as_raw())
        );
        break;
      }
      assert!(
        Instant::now() < deadline,
        "capture helper outlived its worker"
      );
      thread::sleep(Duration::from_millis(2));
    }
  }

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
