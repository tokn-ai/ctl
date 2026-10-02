//! Receiver-confirmed upload speed and installation stall policy.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const SPEED_WINDOW: Duration = Duration::from_secs(10);
const MIN_TRANSFER_IDLE: Duration = Duration::from_secs(30);
const MAX_TRANSFER_IDLE: Duration = Duration::from_mins(5);
const TRANSFER_WINDOW_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteInstallPhase {
  #[default]
  DetectingPlatform,
  VerifyingBundle,
  Connecting,
  Transferring,
  Extracting,
  Checking,
  Activating,
  Complete,
}

impl RemoteInstallPhase {
  #[must_use]
  pub fn label(self) -> &'static str {
    match self {
      Self::DetectingPlatform => "platform detection",
      Self::VerifyingBundle => "bundle verification",
      Self::Connecting => "SSH connection",
      Self::Transferring => "upload",
      Self::Extracting => "archive extraction",
      Self::Checking => "component verification",
      Self::Activating => "activation",
      Self::Complete => "completion",
    }
  }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteInstallProgress {
  pub phase: RemoteInstallPhase,
  pub file_name: Option<String>,
  pub transferred_bytes: u64,
  pub total_bytes: u64,
  pub bytes_per_second: u64,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RemoteInstallStalled {
  pub message: String,
}

/// No total installation deadline: healthy uploads may run as long as needed.
/// Only receiver-confirmed bytes extend the transfer deadline; unchanged
/// reports and a writable local SSH pipe do not count as activity.
pub struct RemoteInstallWatchdog {
  progress: RemoteInstallProgress,
  last_activity: Instant,
  samples: VecDeque<(Instant, u64)>,
  transfer_idle: Duration,
  last_transfer_speed: u64,
}

impl RemoteInstallWatchdog {
  #[must_use]
  pub fn new(now: Instant) -> Self {
    Self {
      progress: RemoteInstallProgress::default(),
      last_activity: now,
      samples: VecDeque::new(),
      transfer_idle: MAX_TRANSFER_IDLE,
      last_transfer_speed: 0,
    }
  }

  /// Computes recent receiver byte rate and checks the current phase's idle
  /// deadline. Call periodically, even when the remote repeats the same report.
  /// Authentication pauses idle accounting while a person answers prompts.
  ///
  /// # Errors
  /// Returns a diagnostic when the receiver or current installation phase has
  /// stopped making progress for its speed-dependent or phase-specific budget.
  pub fn observe(
    &mut self,
    mut next: RemoteInstallProgress,
    now: Instant,
    authenticating: bool,
  ) -> Result<RemoteInstallProgress, RemoteInstallStalled> {
    let stage_changed =
      next.phase != self.progress.phase || next.file_name != self.progress.file_name;
    if stage_changed || authenticating {
      self.last_activity = now;
      self.samples.clear();
      self.samples.push_back((now, next.transferred_bytes));
    }
    if next.phase == RemoteInstallPhase::Transferring {
      if next.transferred_bytes > self.progress.transferred_bytes {
        self.last_activity = now;
        self.samples.push_back((now, next.transferred_bytes));
      }
      while self.samples.len() > 1 && now.duration_since(self.samples[1].0) >= SPEED_WINDOW {
        self.samples.pop_front();
      }
      next.bytes_per_second = self.samples.front().map_or(0, |(at, bytes)| {
        byte_rate(
          next.transferred_bytes.saturating_sub(*bytes),
          now.duration_since(*at),
        )
      });
      if next.transferred_bytes > self.progress.transferred_bytes {
        self.last_transfer_speed = next.bytes_per_second;
        // Allow four 64 KiB receive windows at the recent measured speed,
        // bounded to tolerate jitter without leaving dead uploads forever.
        self.transfer_idle =
          Duration::from_secs((4 * TRANSFER_WINDOW_BYTES).div_ceil(next.bytes_per_second.max(1)))
            .clamp(MIN_TRANSFER_IDLE, MAX_TRANSFER_IDLE);
      }
    } else {
      next.bytes_per_second = 0;
    }
    self.progress = next;
    if !authenticating && now.duration_since(self.last_activity) >= self.idle_budget() {
      let file = self
        .progress
        .file_name
        .as_deref()
        .unwrap_or("remote components");
      let message = if self.progress.phase == RemoteInstallPhase::Transferring {
        format!(
          "Transfer stalled while sending {file}: no new bytes received for {} seconds (last speed {} B/s, {} of {} bytes received).",
          self.transfer_idle.as_secs(),
          self.last_transfer_speed,
          self.progress.transferred_bytes,
          self.progress.total_bytes,
        )
      } else {
        format!(
          "Remote installation stopped making progress during {} ({file}) for {} seconds.",
          self.progress.phase.label(),
          self.idle_budget().as_secs()
        )
      };
      return Err(RemoteInstallStalled { message });
    }
    Ok(self.progress.clone())
  }

  fn idle_budget(&self) -> Duration {
    match self.progress.phase {
      RemoteInstallPhase::Transferring => self.transfer_idle,
      RemoteInstallPhase::DetectingPlatform | RemoteInstallPhase::Connecting => {
        Duration::from_mins(1)
      }
      RemoteInstallPhase::Extracting => Duration::from_mins(2),
      RemoteInstallPhase::VerifyingBundle
      | RemoteInstallPhase::Checking
      | RemoteInstallPhase::Activating
      | RemoteInstallPhase::Complete => Duration::from_secs(30),
    }
  }
}

fn byte_rate(bytes: u64, elapsed: Duration) -> u64 {
  if elapsed.is_zero() {
    return 0;
  }
  let rate = u128::from(bytes) * 1_000_000_000 / elapsed.as_nanos();
  u64::try_from(rate).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
