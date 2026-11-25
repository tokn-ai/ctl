use crate::media::{Capture, CaptureLifetime, Frame};
use std::{
  io::{self, Read, Write},
  sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread,
  time::Duration,
};

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
const NEXT_FRAME_TIMEOUT: Duration = Duration::from_millis(250);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Frames {
  latest: Option<Arc<Frame>>,
  sequence: u64,
  ended: Option<io::Result<()>>,
}

impl Frames {
  fn check(&self) -> io::Result<()> {
    match &self.ended {
      None => Ok(()),
      Some(Ok(())) => Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "capture has ended",
      )),
      Some(Err(error)) => Err(io::Error::new(error.kind(), error.to_string())),
    }
  }
}

#[derive(Default)]
struct Shared {
  frames: Mutex<Frames>,
  changed: Condvar,
  abandoned: AtomicBool,
}

#[derive(Clone, Copy)]
enum Observation {
  First,
  AfterSequence(u64),
  AfterAction,
}

/// Continuously replaces a single frame. Held screenshots can outlive a
/// resize, a subsequent observation, or the complete capture session.
pub(super) struct ActiveCapture<W: Write> {
  lifetime: Option<CaptureLifetime<W>>,
  shared: Arc<Shared>,
  done: mpsc::Receiver<()>,
  next_observation: Observation,
}

impl<W: Write> ActiveCapture<W> {
  pub(super) fn start<R: Read + Send + 'static>(capture: Capture<R, W>) -> io::Result<Self> {
    let (mut reader, lifetime) = capture.split();
    let shared = Arc::new(Shared::default());
    let publisher = Arc::clone(&shared);
    let (finished, done) = mpsc::channel();
    thread::Builder::new()
      .name("ctream-automation-capture".into())
      .spawn(move || {
        let mut next = Frame::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          while !publisher.abandoned.load(Ordering::Acquire) && reader.try_read_frame(&mut next)? {
            let mut frames = publisher.frames.lock().unwrap();
            let previous = frames.latest.replace(Arc::new(next));
            frames.sequence = frames.sequence.wrapping_add(1);
            publisher.changed.notify_all();
            drop(frames);
            next = previous.and_then(Arc::into_inner).unwrap_or_default();
          }
          Ok(())
        }))
        .unwrap_or_else(|_| Err(io::Error::other("capture worker panicked")));
        drop(reader);
        publisher.frames.lock().unwrap().ended = Some(result);
        publisher.changed.notify_all();
        let _ = finished.send(());
      })?;
    Ok(Self {
      lifetime: Some(lifetime),
      shared,
      done,
      next_observation: Observation::First,
    })
  }

  pub(super) fn check(&self) -> io::Result<()> {
    self.shared.frames.lock().unwrap().check()
  }

  pub(super) fn ready(&self) -> io::Result<Arc<Frame>> {
    self.frame(Observation::First).map(|(_, frame)| frame)
  }

  pub(super) fn after_action(&mut self) {
    self.next_observation = Observation::AfterAction;
  }

  pub(super) fn screenshot(&mut self) -> io::Result<Arc<Frame>> {
    let (sequence, frame) = self.frame(self.next_observation)?;
    self.next_observation = Observation::AfterSequence(sequence);
    Ok(frame)
  }

  fn frame(&self, when: Observation) -> io::Result<(u64, Arc<Frame>)> {
    let frames = self.shared.frames.lock().unwrap();
    let previous = match when {
      Observation::First => None,
      Observation::AfterSequence(sequence) => Some(sequence),
      Observation::AfterAction => Some(frames.sequence),
    };
    let timeout = if frames.latest.is_some() {
      NEXT_FRAME_TIMEOUT
    } else {
      FIRST_FRAME_TIMEOUT
    };
    let (frames, _) = self
      .shared
      .changed
      .wait_timeout_while(frames, timeout, |frames| {
        frames.ended.is_none() && (frames.latest.is_none() || previous == Some(frames.sequence))
      })
      .unwrap();
    frames.check()?;
    frames
      .latest
      .clone()
      .map(|frame| (frames.sequence, frame))
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::TimedOut,
          "capture did not deliver a first frame within 30 seconds",
        )
      })
  }

  pub(super) fn finish(&mut self) -> io::Result<()> {
    let Some(mut lifetime) = self.lifetime.take() else {
      return Ok(());
    };
    // Closing stdin lets a healthy peer release its window and flush pending
    // frames. Retain cancellation until the reader sees the remote exit.
    lifetime.close_writer();
    match self.done.recv_timeout(CLOSE_TIMEOUT) {
      Ok(()) => self
        .shared
        .frames
        .lock()
        .unwrap()
        .ended
        .take()
        .unwrap_or(Ok(())),
      Err(mpsc::RecvTimeoutError::Disconnected) => {
        Err(io::Error::other("capture worker exited unexpectedly"))
      }
      Err(mpsc::RecvTimeoutError::Timeout) => {
        self.shared.abandoned.store(true, Ordering::Release);
        drop(lifetime);
        Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "capture did not close after EOF",
        ))
      }
    }
  }
}

impl<W: Write> Drop for ActiveCapture<W> {
  fn drop(&mut self) {
    let _ = self.finish();
  }
}
