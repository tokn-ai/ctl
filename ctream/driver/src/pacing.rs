use std::time::{Duration, Instant};

/// Holds a frame cadence across ordinary scheduling delays without sending bursts.
pub struct FramePacer {
  interval: Duration,
  deadline: Option<Instant>,
}

impl FramePacer {
  #[must_use]
  pub fn new(interval: Duration) -> Self {
    Self {
      interval,
      deadline: None,
    }
  }

  /// Returns the earliest start time for the next frame.
  ///
  /// Each frame should start at or after the previous result. A missed period
  /// starts a new cadence. Ordinary phase correction shortens the next interval
  /// by at most one quarter to absorb small scheduler delays.
  #[must_use]
  pub fn finish_frame(&mut self, started: Instant, finished: Instant) -> Instant {
    let nominal = self.deadline.unwrap_or(started) + self.interval;
    let deadline =
      if finished >= nominal || finished.saturating_duration_since(started) >= self.interval {
        finished + self.interval
      } else {
        let minimum_gap = self.interval - self.interval / 4;
        nominal.max(started + minimum_gap)
      };
    self.deadline = Some(deadline);
    deadline
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ordinary_jitter_does_not_accumulate_and_correction_is_bounded() {
    let base = Instant::now();
    let at = |ms| base + Duration::from_millis(ms);
    let mut pacer = FramePacer::new(Duration::from_millis(40));
    assert_eq!(pacer.finish_frame(at(0), at(2)), at(40));
    assert_eq!(pacer.finish_frame(at(44), at(46)), at(80));
    assert_eq!(pacer.finish_frame(at(95), at(97)), at(125));
    assert_eq!(pacer.finish_frame(at(125), at(127)), at(165));
  }

  #[test]
  fn startup_and_idle_frames_anchor_to_actual_time() {
    let base = Instant::now();
    let at = |ms| base + Duration::from_millis(ms);
    let mut pacer = FramePacer::new(Duration::from_millis(40));
    assert_eq!(pacer.finish_frame(at(300), at(302)), at(340));
    assert_eq!(pacer.finish_frame(at(900), at(902)), at(942));
    assert_eq!(pacer.finish_frame(at(944), at(946)), at(982));
  }

  #[test]
  fn blocked_or_missed_periods_never_catch_up() {
    let base = Instant::now();
    let at = |ms| base + Duration::from_millis(ms);
    let mut pacer = FramePacer::new(Duration::from_millis(40));
    assert_eq!(pacer.finish_frame(at(0), at(41)), at(81));
    assert_eq!(pacer.finish_frame(at(116), at(123)), at(163));
    assert_eq!(pacer.finish_frame(at(163), at(165)), at(203));
  }
}
