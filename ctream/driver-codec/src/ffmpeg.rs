//! `FFmpeg` video streams with one NUT container per capture geometry.

mod command;
mod decode;
mod encode;
mod process;

#[cfg(test)]
mod tests;

use ctream_driver::Frame;
use ctream_proto::VideoCodec;
use std::{
  io,
  sync::{Condvar, Mutex},
  time::{Duration, Instant},
};

pub use decode::Decoder;
pub use encode::Encoder;

/// Discover decoders by running a sample stream through the local `FFmpeg`.
///
/// # Errors
/// Returns an I/O error if `FFmpeg` cannot be launched or its pipes fail.
pub fn available_decoders() -> io::Result<Vec<VideoCodec>> {
  command::available_decoders()
}

const CHECK_INTERVAL: Duration = Duration::from_millis(20);

fn coded_size(display: [usize; 2]) -> [usize; 2] {
  // Encoders use YUV 4:2:0, and the SVT-AV1 provider requires 64x64 or larger.
  display.map(|size| size.next_multiple_of(2).max(64))
}

#[derive(Default)]
struct Latest {
  state: Mutex<LatestState>,
  changed: Condvar,
}

#[derive(Default)]
struct LatestState {
  frame: Option<Frame>,
  spare: Option<Frame>,
  finished: bool,
  error: Option<io::Error>,
}

impl Latest {
  fn publish(&self, frame: &mut Frame) {
    let mut state = self.state.lock().unwrap();
    if !state.finished {
      let reuse = state.frame.take().or_else(|| state.spare.take());
      state.frame = Some(std::mem::replace(frame, reuse.unwrap_or_default()));
      self.changed.notify_one();
    }
  }

  fn finish(&self, result: io::Result<()>) {
    let mut state = self.state.lock().unwrap();
    if !state.finished {
      state.finished = true;
      state.error = result.err();
      self.changed.notify_all();
    }
  }

  fn next(
    &self,
    frame: &mut Frame,
    ready_at: Option<Instant>,
    cancelled: impl Fn() -> bool,
  ) -> io::Result<bool> {
    let mut state = self.state.lock().unwrap();
    loop {
      if cancelled() {
        return Ok(false);
      }
      let remaining = ready_at.map_or(Duration::ZERO, |time| {
        time.saturating_duration_since(Instant::now())
      });
      if remaining.is_zero()
        && let Some(next) = state.frame.take()
      {
        state.spare = Some(std::mem::replace(frame, next));
        return Ok(true);
      }
      if state.finished && state.frame.is_none() {
        return state.error.take().map_or(Ok(false), Err);
      }
      let delay = if state.frame.is_some() {
        remaining.min(CHECK_INTERVAL)
      } else {
        CHECK_INTERVAL
      };
      state = self.changed.wait_timeout(state, delay).unwrap().0;
    }
  }
}
