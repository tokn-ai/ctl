use super::*;

#[test]
fn desktop_adapter_preserves_all_shared_progress_fields_and_phases() {
  for phase in [
    Phase::DetectingPlatform,
    Phase::VerifyingBundle,
    Phase::Connecting,
    Phase::CheckingRemoteInstallation,
    Phase::Transferring,
    Phase::Extracting,
    Phase::Checking,
    Phase::Activating,
    Phase::Complete,
    Phase::Reused,
  ] {
    let progress = Progress {
      phase,
      ssh_upload_fallback: false,
      file_name: Some("bundle.tar.gz".into()),
      transferred_bytes: 1234,
      total_bytes: 5678,
      bytes_per_second: 90,
    };
    assert_eq!(
      desktop_progress(shared_progress(progress.clone()).unwrap()),
      progress
    );
  }
  assert_eq!(
    initial(),
    desktop_progress(RemoteInstallProgress::default())
  );
}

#[test]
fn cached_upload_reports_only_the_current_artifact_byte_counts() {
  use ctl_client::PublishedInstallStage as Stage;
  for (stage, phase, bytes) in [
    (
      Stage::UploadingUpdaterBytes {
        sent: 18,
        total: 20,
      },
      Phase::UploadCachedUpdater,
      (18, 20),
    ),
    (Stage::UploadingRuntime, Phase::UploadCachedRuntime, (0, 0)),
    (
      Stage::UploadingRuntimeBytes {
        sent: 9,
        total: 100,
      },
      Phase::UploadCachedRuntime,
      (9, 100),
    ),
    (
      Stage::InstallingRuntime,
      Phase::RemoteInstallRuntime,
      (0, 0),
    ),
  ] {
    let progress = publication_progress(stage);
    assert_eq!(progress.phase, phase);
    assert_eq!((progress.transferred_bytes, progress.total_bytes), bytes);
  }
}

#[test]
fn fallback_route_survives_skipped_progress_and_transfer_rate_calculation() {
  let (sender, receiver) = watch::channel(initial());
  publish(
    &sender,
    publication_progress(ctl_client::PublishedInstallStage::UploadFallback),
  );
  let mut next = initial();
  next.phase = Phase::Transferring;
  publish(&sender, next);
  let now = Instant::now();
  let event = observe(
    &mut RemoteInstallWatchdog::new(now),
    receiver.borrow().clone(),
    now,
    false,
  )
  .unwrap();
  assert_eq!(event.phase, Phase::Transferring);
  assert!(event.ssh_upload_fallback);
}

#[test]
fn source_preparation_can_exceed_the_installation_deadline_without_disabling_ssh_checks() {
  let start = Instant::now();
  let mut watchdog = RemoteInstallWatchdog::new(start);
  let mut built = start;
  for phase in [
    Phase::PreparingBundle,
    Phase::DownloadingBundle,
    Phase::BuildingBundle,
    Phase::CheckingBundle,
  ] {
    let mut preparation = initial();
    preparation.phase = phase;
    observe(&mut watchdog, preparation.clone(), built, false).unwrap();
    built += Duration::from_mins(10);
    assert_eq!(
      observe(&mut watchdog, preparation, built, false)
        .unwrap()
        .phase,
      phase
    );
  }
  let mut connecting = initial();
  connecting.phase = Phase::Connecting;
  observe(&mut watchdog, connecting.clone(), built, false).unwrap();
  assert!(
    observe(
      &mut watchdog,
      connecting,
      built + Duration::from_secs(61),
      false
    )
    .is_err()
  );
}

#[tokio::test]
async fn completion_preserves_confirmed_bundle_reuse_without_transfer() {
  let (updates, receiver) = watch::channel(initial());
  let reports = std::sync::Mutex::new(Vec::new());
  monitor(
    async {
      let mut reused = initial();
      reused.phase = Phase::Reused;
      updates.send_replace(reused);
      Ok(())
    },
    receiver,
    |event| {
      reports.lock().unwrap().push(event);
      Ok(())
    },
    || false,
  )
  .await
  .unwrap();
  let reports = reports.into_inner().unwrap();
  let last = reports.last().unwrap();
  assert_eq!(last.phase, Phase::Reused);
  assert_eq!((last.transferred_bytes, last.total_bytes), (0, 0));
}

#[tokio::test]
async fn cancelling_the_monitor_drops_an_in_progress_source_build() {
  use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  };
  struct Build(Arc<AtomicBool>);
  impl Drop for Build {
    fn drop(&mut self) {
      self.0.store(true, Ordering::SeqCst);
    }
  }
  let dropped = Arc::new(AtomicBool::new(false));
  let build = Build(Arc::clone(&dropped));
  let mut preparing = initial();
  preparing.phase = Phase::PreparingBundle;
  let (_updates, receiver) = watch::channel(preparing);
  let (started, running) = tokio::sync::oneshot::channel();
  let task = tokio::spawn(monitor(
    async move {
      let _build = build;
      let _ = started.send(());
      std::future::pending::<CommandResult<()>>().await
    },
    receiver,
    |_| Ok(()),
    || false,
  ));
  running.await.unwrap();
  task.abort();
  assert!(task.await.unwrap_err().is_cancelled());
  assert!(dropped.load(Ordering::SeqCst));
}
