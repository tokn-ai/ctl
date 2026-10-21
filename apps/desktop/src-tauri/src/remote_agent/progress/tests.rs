use super::*;

#[test]
fn desktop_adapter_preserves_all_shared_progress_fields_and_phases() {
  for phase in [
    Phase::DetectingPlatform,
    Phase::VerifyingBundle,
    Phase::Connecting,
    Phase::Transferring,
    Phase::Extracting,
    Phase::Checking,
    Phase::Activating,
    Phase::Complete,
  ] {
    let progress = Progress {
      phase,
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
fn source_preparation_can_exceed_the_installation_deadline_without_disabling_ssh_checks() {
  let start = Instant::now();
  let mut watchdog = RemoteInstallWatchdog::new(start);
  let mut preparation = initial();
  preparation.phase = Phase::PreparingBundle;
  observe(&mut watchdog, preparation.clone(), start, false).unwrap();
  let built = start + Duration::from_mins(10);
  assert_eq!(
    observe(&mut watchdog, preparation, built, false)
      .unwrap()
      .phase,
    Phase::PreparingBundle
  );
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
