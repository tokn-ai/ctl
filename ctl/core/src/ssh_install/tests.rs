use super::*;

#[test]
fn bundle_ids_cannot_change_the_fixed_script_or_installation_path() {
  for bundle_id in ["", "../escape", "v1/release", "line\nbreak", "$(whoami)"] {
    assert!(matches!(
      install_script(bundle_id, 12),
      Err(CoreError::InvalidAgentBundleId(_))
    ));
  }
  assert!(install_script("0.1.0-dev.0123456789ab", 12).is_ok());
}

#[tokio::test]
async fn progress_requires_monotonic_receiver_bytes_and_complete_upload() {
  use std::sync::Mutex;
  let events = Mutex::new(Vec::new());
  let valid = b"ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 5\nctl-install-progress-v1 receiving 10\nctl-install-progress-v1 extracting\nctl-install-progress-v1 checking ctl-agent\nctl-install-progress-v1 checking rmuxd\nctl-install-progress-v1 checking taskd\nctl-install-progress-v1 activating\nctl-install-v1\n";
  read_progress(&valid[..], 10, &|event| events.lock().unwrap().push(event))
    .await
    .unwrap();
  assert_eq!(
    events.lock().unwrap()[2],
    RemoteInstallEvent::Receiving { received_bytes: 10 }
  );
  for invalid in [
    "ctl-install-progress-v1 receiving 11\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 10\nctl-install-progress-v1 receiving 11\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 10\nctl-install-progress-v1 receiving 9\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 5\nctl-install-progress-v1 receiving 4\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 5\nctl-install-progress-v1 extracting\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 10\n",
    "banner\nctl-install-progress-v1 receiving 10\nctl-install-v1\n",
  ] {
    assert!(
      read_progress(invalid.as_bytes(), 10, &|_| {})
        .await
        .is_err()
    );
  }
  assert!(
    read_progress(&vec![b'x'; 16_384][..], 10, &|_| {})
      .await
      .is_err()
  );
}

#[cfg(unix)]
struct BundleFixture {
  directory: std::path::PathBuf,
  archive: Vec<u8>,
}

#[cfg(unix)]
impl BundleFixture {
  fn new() -> Self {
    let directory = std::env::temp_dir().join(format!(
      "ctl-core-install-{}",
      uuid::Uuid::new_v4().simple()
    ));
    let source = directory.join("source");
    let archive = directory.join("bundle.tar.gz");
    std::fs::create_dir_all(&source).unwrap();
    for binary in ["ctl-agent", "rmuxd", "taskd"] {
      std::fs::write(source.join(binary), binary).unwrap();
    }
    assert!(
      std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(source)
        .args(["ctl-agent", "rmuxd", "taskd"])
        .status()
        .unwrap()
        .success()
    );
    Self {
      directory,
      archive: std::fs::read(archive).unwrap(),
    }
  }

  fn command(&self, expected_bytes: usize) -> Command {
    let mut command = Command::new("sh");
    command
      .args(["-c", &install_script("0.1.0-test", expected_bytes).unwrap()])
      .env("HOME", self.directory.join("home"))
      .env("XDG_DATA_HOME", self.directory.join("unused-data"));
    command
  }
}

#[cfg(unix)]
impl Drop for BundleFixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_reports_receiver_progress_and_activates_executable_components() {
  use std::os::unix::fs::PermissionsExt as _;
  use std::sync::Mutex;
  let fixture = BundleFixture::new();
  let events = Mutex::new(Vec::new());
  let identity_path = fixture.directory.join("home/.tokn/ctl/remote-id");
  std::fs::create_dir_all(identity_path.parent().unwrap()).unwrap();
  let remote_id = uuid::Uuid::new_v4().to_string();
  std::fs::write(&identity_path, &remote_id).unwrap();
  tokio::time::timeout(
    std::time::Duration::from_secs(10),
    run_install_command(
      fixture.command(fixture.archive.len()),
      &fixture.archive,
      |event| events.lock().unwrap().push(event),
    ),
  )
  .await
  .unwrap()
  .unwrap();
  let events = events.into_inner().unwrap();
  assert_eq!(
    events.first(),
    Some(&RemoteInstallEvent::Receiving { received_bytes: 0 })
  );
  assert!(events.contains(&RemoteInstallEvent::Receiving {
    received_bytes: fixture.archive.len() as u64
  }));
  assert!(events.contains(&RemoteInstallEvent::Extracting));
  assert_eq!(events.last(), Some(&RemoteInstallEvent::Complete));
  assert_eq!(std::fs::read_to_string(identity_path).unwrap(), remote_id);
  assert!(!fixture.directory.join("unused-data").exists());
  let current = fixture.directory.join("home/.tokn/ctl/current");
  assert_eq!(
    std::fs::read_link(&current).unwrap(),
    std::path::PathBuf::from("versions/0.1.0-test")
  );
  for binary in ["ctl-agent", "rmuxd", "taskd"] {
    assert!(events.contains(&RemoteInstallEvent::Checking { file_name: binary }));
    assert_eq!(
      std::fs::read_to_string(current.join(binary)).unwrap(),
      binary
    );
    assert_eq!(
      std::fs::metadata(current.join(binary))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o700
    );
  }
}

#[cfg(unix)]
#[tokio::test]
async fn truncated_upload_does_not_activate_and_cleans_temporary_files() {
  let fixture = BundleFixture::new();
  let result = tokio::time::timeout(
    std::time::Duration::from_secs(10),
    run_install_command(
      fixture.command(fixture.archive.len() + 1),
      &fixture.archive,
      |_| {},
    ),
  )
  .await
  .unwrap();
  assert!(matches!(result, Err(CoreError::SshCommandFailed { .. })));
  assert!(!fixture.directory.join("home/.tokn/ctl/current").exists());
  assert_eq!(
    std::fs::read_dir(fixture.directory.join("home/.tokn/ctl/versions"))
      .unwrap()
      .count(),
    0
  );
}

#[cfg(unix)]
#[tokio::test]
async fn early_remote_failure_preserves_diagnostics_instead_of_broken_pipe() {
  let mut command = Command::new("sh");
  command.args(["-c", "printf 'Permission denied by fixture' >&2; exit 1"]);
  let result = run_install_command(command, &vec![0; 1024 * 1024], |_| {}).await;
  assert!(
    matches!(result, Err(CoreError::SshCommandFailed { diagnostic, .. }) if diagnostic.contains("Permission denied"))
  );
}
