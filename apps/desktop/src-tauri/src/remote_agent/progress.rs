use std::future::Future;
use std::time::{Duration, Instant};

use ctl_client::{RemoteInstallPhase, RemoteInstallProgress, RemoteInstallWatchdog};
use tokio::sync::watch;

use crate::dto::{RemoteAgentInstallPhase as Phase, RemoteAgentInstallProgressDto as Progress};
use crate::error::{CommandErrorDto, CommandResult};

pub(super) fn initial() -> Progress {
  desktop_progress(RemoteInstallProgress::default())
}

fn shared_progress(progress: Progress) -> RemoteInstallProgress {
  RemoteInstallProgress {
    phase: match progress.phase {
      Phase::DetectingPlatform => RemoteInstallPhase::DetectingPlatform,
      Phase::VerifyingBundle => RemoteInstallPhase::VerifyingBundle,
      Phase::Connecting => RemoteInstallPhase::Connecting,
      Phase::Transferring => RemoteInstallPhase::Transferring,
      Phase::Extracting => RemoteInstallPhase::Extracting,
      Phase::Checking => RemoteInstallPhase::Checking,
      Phase::Activating => RemoteInstallPhase::Activating,
      Phase::Complete => RemoteInstallPhase::Complete,
    },
    file_name: progress.file_name,
    transferred_bytes: progress.transferred_bytes,
    total_bytes: progress.total_bytes,
    bytes_per_second: progress.bytes_per_second,
  }
}

pub(super) fn desktop_progress(progress: RemoteInstallProgress) -> Progress {
  Progress {
    phase: match progress.phase {
      RemoteInstallPhase::DetectingPlatform => Phase::DetectingPlatform,
      RemoteInstallPhase::VerifyingBundle => Phase::VerifyingBundle,
      RemoteInstallPhase::Connecting => Phase::Connecting,
      RemoteInstallPhase::Transferring => Phase::Transferring,
      RemoteInstallPhase::Extracting => Phase::Extracting,
      RemoteInstallPhase::Checking => Phase::Checking,
      RemoteInstallPhase::Activating => Phase::Activating,
      RemoteInstallPhase::Complete => Phase::Complete,
    },
    file_name: progress.file_name,
    transferred_bytes: progress.transferred_bytes,
    total_bytes: progress.total_bytes,
    bytes_per_second: progress.bytes_per_second,
  }
}

pub(super) async fn monitor<T>(
  install: impl Future<Output = CommandResult<T>>,
  mut updates: watch::Receiver<Progress>,
  on_progress: impl Fn(Progress) -> CommandResult<()>,
  authenticating: impl Fn() -> bool,
) -> CommandResult<T> {
  tokio::pin!(install);
  let mut watchdog = RemoteInstallWatchdog::new(Instant::now());
  let mut tick = tokio::time::interval(Duration::from_millis(500));
  loop {
    tokio::select! {
      result = &mut install => {
        if result.is_ok() {
          let mut complete = updates.borrow().clone();
          complete.phase = Phase::Complete;
          complete.bytes_per_second = 0;
          on_progress(complete)?;
        }
        return result;
      }
      _ = tick.tick() => {}
      changed = updates.changed() => {
        if changed.is_err() { return install.await; }
      }
    }
    let progress = watchdog
      .observe(
        shared_progress(updates.borrow().clone()),
        Instant::now(),
        authenticating(),
      )
      .map_err(|error| CommandErrorDto::new("remote_agent_install_stalled", error.to_string()))?;
    on_progress(desktop_progress(progress))?;
  }
}

#[cfg(test)]
mod tests;
