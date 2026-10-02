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
      desktop_progress(shared_progress(progress.clone())),
      progress
    );
  }
  assert_eq!(
    initial(),
    desktop_progress(RemoteInstallProgress::default())
  );
}
