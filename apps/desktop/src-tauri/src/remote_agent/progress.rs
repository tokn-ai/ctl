use std::collections::VecDeque;
use std::future::Future;
use std::time::{Duration, Instant};

use tauri::ipc::Channel;
use tokio::sync::watch;

use crate::dto::{RemoteAgentInstallPhase as Phase, RemoteAgentInstallProgressDto as Progress};
use crate::error::{CommandErrorDto, CommandResult};

const SPEED_WINDOW: Duration = Duration::from_secs(10);
const MIN_TRANSFER_IDLE: Duration = Duration::from_secs(30);
const MAX_TRANSFER_IDLE: Duration = Duration::from_mins(5);
const TRANSFER_WINDOW_BYTES: u64 = 64 * 1024;

pub(super) fn initial() -> Progress {
  Progress {
    phase: Phase::DetectingPlatform,
    file_name: None,
    transferred_bytes: 0,
    total_bytes: 0,
    bytes_per_second: 0,
  }
}

/// No total installation deadline: healthy uploads may run as long as needed.
/// Only receiver-confirmed bytes extend the transfer deadline; unchanged
/// progress reports and a writable local SSH pipe do not count as activity.
struct InstallWatchdog {
  progress: Progress,
  last_activity: Instant,
  samples: VecDeque<(Instant, u64)>,
  transfer_idle: Duration,
  last_transfer_speed: u64,
}

impl InstallWatchdog {
  fn new(now: Instant) -> Self {
    Self {
      progress: initial(),
      last_activity: now,
      samples: VecDeque::new(),
      transfer_idle: MAX_TRANSFER_IDLE,
      last_transfer_speed: 0,
    }
  }

  fn observe(
    &mut self,
    mut next: Progress,
    now: Instant,
    authenticating: bool,
  ) -> CommandResult<Progress> {
    let stage_changed =
      next.phase != self.progress.phase || next.file_name != self.progress.file_name;
    if stage_changed || authenticating {
      self.last_activity = now;
      self.samples.clear();
      self.samples.push_back((now, next.transferred_bytes));
    }
    if next.phase == Phase::Transferring {
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
      let message = if self.progress.phase == Phase::Transferring {
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
          phase_label(self.progress.phase),
          self.idle_budget().as_secs()
        )
      };
      return Err(CommandErrorDto::new(
        "remote_agent_install_stalled",
        message,
      ));
    }
    Ok(self.progress.clone())
  }

  fn idle_budget(&self) -> Duration {
    match self.progress.phase {
      Phase::Transferring => self.transfer_idle,
      Phase::DetectingPlatform | Phase::Connecting => Duration::from_mins(1),
      Phase::Extracting => Duration::from_mins(2),
      Phase::VerifyingBundle | Phase::Checking | Phase::Activating | Phase::Complete => {
        Duration::from_secs(30)
      }
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

fn phase_label(phase: Phase) -> &'static str {
  match phase {
    Phase::DetectingPlatform => "platform detection",
    Phase::VerifyingBundle => "bundle verification",
    Phase::Connecting => "SSH connection",
    Phase::Transferring => "upload",
    Phase::Extracting => "archive extraction",
    Phase::Checking => "component verification",
    Phase::Activating => "activation",
    Phase::Complete => "completion",
  }
}

pub(super) async fn monitor<T>(
  install: impl Future<Output = CommandResult<T>>,
  mut updates: watch::Receiver<Progress>,
  channel: &Channel<Progress>,
  authenticating: impl Fn() -> bool,
) -> CommandResult<T> {
  tokio::pin!(install);
  let mut watchdog = InstallWatchdog::new(Instant::now());
  let mut tick = tokio::time::interval(Duration::from_millis(500));
  loop {
    tokio::select! {
      result = &mut install => {
        if result.is_ok() {
          let mut complete = updates.borrow().clone();
          complete.phase = Phase::Complete;
          complete.bytes_per_second = 0;
          let _ = channel.send(complete);
        }
        return result;
      }
      _ = tick.tick() => {}
      changed = updates.changed() => {
        if changed.is_err() { return install.await; }
      }
    }
    let progress = watchdog.observe(updates.borrow().clone(), Instant::now(), authenticating())?;
    channel
      .send(progress)
      .map_err(|error| CommandErrorDto::new("remote_agent_progress_closed", error.to_string()))?;
  }
}

#[cfg(test)]
mod tests;
