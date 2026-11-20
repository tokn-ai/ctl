use std::future::Future;
use std::time::{Duration, Instant};

use ctl_client::{RemoteInstallPhase, RemoteInstallProgress, RemoteInstallWatchdog};
use tokio::sync::watch;

use crate::dto::{RemoteAgentInstallPhase as Phase, RemoteAgentInstallProgressDto as Progress};
use crate::error::{CommandErrorDto, CommandResult};

pub(super) fn initial() -> Progress {
  desktop_progress(RemoteInstallProgress::default())
}

pub(super) fn publish(updates: &watch::Sender<Progress>, mut progress: Progress) {
  // Watch subscribers can skip short-lived stages between two byte reports.
  // Keep the fallback route visible for the rest of this host's operation.
  updates.send_modify(|previous| {
    progress.ssh_upload_fallback |= previous.ssh_upload_fallback;
    *previous = progress;
  });
}

pub(super) fn publication_progress(stage: ctl_client::PublishedInstallStage) -> Progress {
  use ctl_client::PublishedInstallStage as Stage;
  let (transferred_bytes, total_bytes) = match stage {
    Stage::UploadingUpdaterBytes { sent, total } | Stage::UploadingRuntimeBytes { sent, total } => {
      (sent, total)
    }
    _ => (0, 0),
  };
  Progress {
    phase: match stage {
      Stage::CheckingLocalCache => Phase::CheckingLocalCache,
      Stage::UsingLocalCache => Phase::UsingLocalCache,
      Stage::CheckingUpdater => Phase::CheckingRemoteInstallation,
      Stage::DownloadingUpdater => Phase::RemoteDownloadUpdater,
      Stage::UploadingUpdater | Stage::UploadingUpdaterBytes { .. } => Phase::UploadCachedUpdater,
      Stage::UsingInstalledUpdater => Phase::UsingRemoteUpdater,
      Stage::InstallingRuntime => Phase::RemoteInstallRuntime,
      Stage::UploadingRuntime | Stage::UploadingRuntimeBytes { .. } => Phase::UploadCachedRuntime,
      Stage::ReusedRuntime => Phase::Reused,
      Stage::UploadFallback => Phase::UploadFallback,
    },
    transferred_bytes,
    total_bytes,
    ssh_upload_fallback: stage == Stage::UploadFallback,
    ..initial()
  }
}

fn shared_progress(progress: Progress) -> Option<RemoteInstallProgress> {
  Some(RemoteInstallProgress {
    phase: match progress.phase {
      Phase::DetectingPlatform => RemoteInstallPhase::DetectingPlatform,
      Phase::PreparingBundle
      | Phase::DownloadingBundle
      | Phase::BuildingBundle
      | Phase::CheckingLocalCache
      | Phase::UsingLocalCache
      | Phase::RemoteDownloadUpdater
      | Phase::UsingRemoteUpdater
      | Phase::RemoteInstallRuntime
      | Phase::UploadCachedUpdater
      | Phase::UploadCachedRuntime
      | Phase::UploadFallback
      | Phase::CheckingBundle => return None,
      Phase::VerifyingBundle => RemoteInstallPhase::VerifyingBundle,
      Phase::Connecting => RemoteInstallPhase::Connecting,
      Phase::CheckingRemoteInstallation => RemoteInstallPhase::CheckingRemoteInstallation,
      Phase::Transferring => RemoteInstallPhase::Transferring,
      Phase::Extracting => RemoteInstallPhase::Extracting,
      Phase::Checking => RemoteInstallPhase::Checking,
      Phase::Activating => RemoteInstallPhase::Activating,
      Phase::Complete => RemoteInstallPhase::Complete,
      Phase::Reused => RemoteInstallPhase::Reused,
    },
    file_name: progress.file_name,
    transferred_bytes: progress.transferred_bytes,
    total_bytes: progress.total_bytes,
    bytes_per_second: progress.bytes_per_second,
  })
}

fn observe(
  watchdog: &mut RemoteInstallWatchdog,
  progress: Progress,
  now: Instant,
  authenticating: bool,
) -> CommandResult<Progress> {
  let Some(shared) = shared_progress(progress.clone()) else {
    // Compilation has no SSH receiver and may take minutes on a cold cache.
    // Downloads enforce their own network timeouts. Keep preparation cancellable
    // without treating a healthy build as a stalled 30-second upload phase.
    *watchdog = RemoteInstallWatchdog::new(now);
    return Ok(progress);
  };
  watchdog
    .observe(shared, now, authenticating)
    .map(|observed| Progress {
      ssh_upload_fallback: progress.ssh_upload_fallback,
      ..desktop_progress(observed)
    })
    .map_err(|error| CommandErrorDto::new("remote_agent_install_stalled", error.to_string()))
}

pub(super) fn desktop_progress(progress: RemoteInstallProgress) -> Progress {
  Progress {
    ssh_upload_fallback: false,
    phase: match progress.phase {
      RemoteInstallPhase::DetectingPlatform => Phase::DetectingPlatform,
      RemoteInstallPhase::VerifyingBundle => Phase::VerifyingBundle,
      RemoteInstallPhase::Connecting => Phase::Connecting,
      RemoteInstallPhase::CheckingRemoteInstallation => Phase::CheckingRemoteInstallation,
      RemoteInstallPhase::Transferring => Phase::Transferring,
      RemoteInstallPhase::Extracting => Phase::Extracting,
      RemoteInstallPhase::Checking => Phase::Checking,
      RemoteInstallPhase::Activating => Phase::Activating,
      RemoteInstallPhase::Complete => Phase::Complete,
      RemoteInstallPhase::Reused => Phase::Reused,
    },
    file_name: progress.file_name,
    transferred_bytes: progress.transferred_bytes,
    total_bytes: progress.total_bytes,
    bytes_per_second: progress.bytes_per_second,
  }
}

pub(crate) fn preparation_progress(
  progress: ctl_client::component_update::PreparationProgress,
) -> Progress {
  use ctl_client::component_update::PreparationPhase;
  Progress {
    ssh_upload_fallback: false,
    phase: match progress.phase {
      PreparationPhase::Resolving => Phase::PreparingBundle,
      PreparationPhase::Downloading => Phase::DownloadingBundle,
      PreparationPhase::Building => Phase::BuildingBundle,
      PreparationPhase::Verifying => Phase::CheckingBundle,
    },
    file_name: Some(progress.detail),
    transferred_bytes: progress.received_bytes,
    total_bytes: progress.total_bytes,
    bytes_per_second: 0,
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
          if complete.phase != Phase::Reused {
            complete.phase = Phase::Complete;
          }
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
    on_progress(observe(
      &mut watchdog,
      updates.borrow().clone(),
      Instant::now(),
      authenticating(),
    )?)?;
  }
}

#[cfg(test)]
mod tests;
