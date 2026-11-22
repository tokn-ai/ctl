use std::{
  io::{self, BufRead, BufReader, Read, Write},
  num::NonZeroUsize,
  sync::{Condvar, Mutex},
  time::{Duration, Instant},
};

#[derive(Default)]
enum Status {
  #[default]
  Reading,
  Finished(Option<io::Error>),
}

#[derive(Default)]
struct State {
  pending: Option<Vec<u8>>,
  spare: Vec<u8>,
  status: Status,
}

/// One complete pending frame. The child reader keeps draining while the
/// transport writes a frame or waits for the next send.
#[derive(Default)]
pub(super) struct LatestFrames {
  state: Mutex<State>,
  changed: Condvar,
}

impl LatestFrames {
  pub(super) fn read_from(&self, reader: impl Read, frame_bytes: NonZeroUsize) {
    let mut reader = BufReader::new(reader);
    let mut frame = Vec::new();
    let result = (|| {
      loop {
        match reader.fill_buf() {
          Ok([]) => return Ok(()),
          Ok(_) => {}
          Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
          Err(error) => return Err(error),
        }
        frame.resize(frame_bytes.get(), 0);
        reader.read_exact(&mut frame)?;
        let Some(reusable) = self.publish(frame) else {
          return Ok(());
        };
        frame = reusable;
      }
    })();
    self.finish(result.err());
  }

  fn publish(&self, frame: Vec<u8>) -> Option<Vec<u8>> {
    let mut state = self.state.lock().unwrap();
    if !matches!(state.status, Status::Reading) {
      return None;
    }
    if let Some(previous) = state.pending.replace(frame) {
      return Some(previous);
    }
    self.changed.notify_one();
    Some(std::mem::take(&mut state.spare))
  }

  fn finish(&self, error: Option<io::Error>) {
    let mut state = self.state.lock().unwrap();
    if matches!(state.status, Status::Reading) {
      state.status = Status::Finished(error);
      self.changed.notify_one();
    }
  }

  pub(super) fn cancel(&self) {
    let mut state = self.state.lock().unwrap();
    state.pending = None;
    state.status = Status::Finished(None);
    self.changed.notify_one();
  }

  fn next(&self, not_before: Instant) -> io::Result<Option<Vec<u8>>> {
    let mut state = self.state.lock().unwrap();
    loop {
      if state.pending.is_some() {
        let wait = not_before.saturating_duration_since(Instant::now());
        if wait.is_zero() {
          return Ok(state.pending.take());
        }
        state = self.changed.wait_timeout(state, wait).unwrap().0;
      } else {
        if let Status::Finished(error) = &mut state.status {
          return error.take().map_or(Ok(None), Err);
        }
        state = self.changed.wait(state).unwrap();
      }
    }
  }

  pub(super) fn write_to(&self, output: &mut impl Write, interval: Duration) -> io::Result<()> {
    let mut not_before = Instant::now();
    while let Some(frame) = self.next(not_before)? {
      output.write_all(&frame)?;
      output.flush()?;
      // A blocked write can finish long after it began. Start the next
      // interval here so sending never catches up to an old deadline.
      not_before = Instant::now() + interval;
      self.state.lock().unwrap().spare = frame;
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{sync::mpsc, thread};

  struct BlockFirstWrite {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    writes: Vec<(Instant, Vec<u8>)>,
  }

  impl Write for BlockFirstWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      if self.writes.is_empty() {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(2)).unwrap();
      }
      self.writes.push((Instant::now(), bytes.to_vec()));
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  #[test]
  fn blocked_output_sends_only_the_latest_pending_frame_after_an_interval() {
    let frames = LatestFrames::default();
    let interval = Duration::from_millis(25);
    let (entered, blocked) = mpsc::channel();
    let (release, resumed) = mpsc::channel();
    let mut output = BlockFirstWrite {
      entered,
      release: resumed,
      writes: Vec::new(),
    };
    frames.publish(vec![1, 1, 1]).unwrap();
    thread::scope(|scope| {
      scope.spawn(|| frames.write_to(&mut output, interval).unwrap());
      blocked.recv_timeout(Duration::from_secs(2)).unwrap();
      frames.read_from(&b"222333444"[..], NonZeroUsize::new(3).unwrap());
      release.send(()).unwrap();
    });
    let [(first, a), (next, b)] = output.writes.as_slice() else {
      panic!("expected the in-flight frame and one current frame");
    };
    assert_eq!(a, &[1, 1, 1]);
    assert_eq!(b, b"444");
    assert!(*next - *first >= interval);
  }

  #[test]
  fn a_truncated_frame_is_never_written_to_the_transport() {
    let frames = LatestFrames::default();
    frames.read_from(&b"abcdefg"[..], NonZeroUsize::new(3).unwrap());
    let mut output = Vec::new();
    let error = frames.write_to(&mut output, Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(output, b"def");
  }
}
