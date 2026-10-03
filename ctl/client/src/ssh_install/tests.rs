use super::*;

const VALID_PROGRESS: &[u8] = b"ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 5\nctl-install-progress-v1 receiving 10\nctl-install-progress-v1 extracting\nctl-install-progress-v1 checking ctl-agent\nctl-install-progress-v1 checking ctmuxd\nctl-install-progress-v1 checking ctl-taskd\nctl-install-progress-v1 activating\nctl-install-v1\n";

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
  read_progress(VALID_PROGRESS, 10, &|event| {
    events.lock().unwrap().push(event);
  })
  .await
  .unwrap();
  assert_eq!(
    events.lock().unwrap()[2],
    RemoteInstallEvent::Receiving { received_bytes: 10 }
  );
  for invalid in [
    "ctl-install-progress-v1 receiving 11\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 10\nctl-install-progress-v1 receiving 11\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 10\nctl-install-progress-v1 receiving 9\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 5\nctl-install-progress-v1 receiving 4\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 5\nctl-install-progress-v1 extracting\nctl-install-v1\n",
    "ctl-install-progress-v1 receiving 0\nctl-install-progress-v1 receiving 10\n",
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

#[tokio::test]
async fn progress_accepts_startup_output_only_before_the_initial_marker() {
  use std::sync::Mutex;
  for startup in [
    b"Welcome to the server\n".as_slice(),
    b"\x1b[32mstartup without a final newline\x1b[0m".as_slice(),
    b"\xff\x80\0startup output\n".as_slice(),
  ] {
    let output = [startup, VALID_PROGRESS].concat();
    let events = Mutex::new(Vec::new());
    read_progress(&output[..], 10, &|event| events.lock().unwrap().push(event))
      .await
      .unwrap();
    let events = events.into_inner().unwrap();
    assert_eq!(
      events.first(),
      Some(&RemoteInstallEvent::Receiving { received_bytes: 0 })
    );
    assert_eq!(events.last(), Some(&RemoteInstallEvent::Complete));
    assert_eq!(events.len(), 9);
  }

  for late_noise in [
    b"banner\n".as_slice(),
    b"\xff\0\n".as_slice(),
    b"ctl-install-progress-v2 receiving 0\n".as_slice(),
  ] {
    let output = [INITIAL_PROGRESS_MARKER, late_noise, VALID_PROGRESS].concat();
    assert_eq!(
      read_progress(&output[..], 10, &|_| {})
        .await
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidData
    );
  }
}

#[tokio::test]
async fn progress_rejects_missing_or_unsupported_initial_markers() {
  for invalid in [
    b"ctl-install-progress-v1 receiving 5\n".as_slice(),
    b"ctl-install-progress-v2 receiving 0\n".as_slice(),
    b"ctl-install-v1\n".as_slice(),
    b"ctl-install-progress-v1 receiving 0".as_slice(),
    b"shell startup only\n".as_slice(),
  ] {
    let events = std::sync::Mutex::new(Vec::new());
    assert!(
      read_progress(invalid, 10, &|event| events.lock().unwrap().push(event))
        .await
        .is_err()
    );
    assert_eq!(events.into_inner().unwrap(), []);
  }
}

#[tokio::test]
async fn progress_drains_output_after_the_startup_limit_is_exceeded() {
  use tokio::io::AsyncWriteExt as _;
  let (mut writer, reader) = tokio::io::duplex(64);
  let write = tokio::spawn(async move {
    writer.write_all(&vec![b'x'; 128 * 1024]).await.unwrap();
    writer.write_all(VALID_PROGRESS).await.unwrap();
  });
  let result = tokio::time::timeout(
    std::time::Duration::from_secs(5),
    read_progress(reader, 10, &|_| {}),
  )
  .await
  .expect("invalid startup must drain stdout so the writer can finish");
  assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
  write.await.unwrap();
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
      "ctl-client-install-{}",
      uuid::Uuid::new_v4().simple()
    ));
    let source = directory.join("source");
    std::fs::create_dir_all(&source).unwrap();
    for binary in ["ctl-agent", "ctmuxd", "ctl-taskd"] {
      std::fs::write(source.join(binary), binary).unwrap();
    }
    let mut fixture = Self {
      directory,
      archive: Vec::new(),
    };
    fixture.rebuild_archive();
    fixture
  }

  fn rebuild_archive(&mut self) {
    let source = self.directory.join("source");
    let archive = self.directory.join("bundle.tar.gz");
    let mut files = vec!["ctl-agent", "ctmuxd", "ctl-taskd"];
    if source.join("manifest.json").exists() {
      files.push("manifest.json");
    }
    assert!(
      std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(source)
        .args(files)
        .status()
        .unwrap()
        .success()
    );
    self.archive = std::fs::read(archive).unwrap();
  }

  async fn install(&self, bundle_id: &str) -> Result<(), CoreError> {
    tokio::time::timeout(
      std::time::Duration::from_secs(10),
      run_install_command(
        self.command_for(bundle_id, self.archive.len()),
        &self.archive,
        |_| {},
      ),
    )
    .await
    .unwrap()
  }

  fn assert_staging_clean(&self) {
    let base = self.directory.join("home/.tokn/ctl");
    for directory in [&base, &base.join("versions")] {
      for entry in std::fs::read_dir(directory).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_str().unwrap();
        assert!(!name.starts_with(".install-"), "left staging entry {name}");
        assert!(
          !name.starts_with(".current-"),
          "left activation entry {name}"
        );
      }
    }
  }

  fn command(&self, expected_bytes: usize) -> Command {
    self.command_for("0.1.0-test", expected_bytes)
  }

  fn command_for(&self, bundle_id: &str, expected_bytes: usize) -> Command {
    self.command_with_startup(bundle_id, expected_bytes, "")
  }

  fn command_with_startup(&self, bundle_id: &str, expected_bytes: usize, startup: &str) -> Command {
    let script = format!(
      "printf '%s' \"$CTL_INSTALL_TEST_STARTUP\"; {}",
      install_script(bundle_id, expected_bytes).unwrap()
    );
    let mut command = Command::new("sh");
    command
      .args(["-c", &script])
      .env("CTL_INSTALL_TEST_STARTUP", startup)
      .env("HOME", self.directory.join("home"))
      .env("XDG_DATA_HOME", self.directory.join("unused-data"));
    command
  }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_activates_components_despite_shell_startup_output() {
  let fixture = BundleFixture::new();
  tokio::time::timeout(
    std::time::Duration::from_secs(10),
    run_install_command(
      fixture.command_with_startup(
        "0.1.0-test",
        fixture.archive.len(),
        "Welcome to this server\n\x1b[32mloading profile\x1b[0m",
      ),
      &fixture.archive,
      |_| {},
    ),
  )
  .await
  .unwrap()
  .unwrap();
  assert_eq!(
    std::fs::read_link(fixture.directory.join("home/.tokn/ctl/current")).unwrap(),
    std::path::PathBuf::from("versions/0.1.0-test")
  );
}

#[cfg(unix)]
impl Drop for BundleFixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_replaces_an_existing_current_directory_symlink() {
  let fixture = BundleFixture::new();
  let base = fixture.directory.join("home/.tokn/ctl");
  let old = base.join("versions/0.1.0-old");
  std::fs::create_dir_all(&old).unwrap();
  std::os::unix::fs::symlink("versions/0.1.0-old", base.join("current")).unwrap();

  run_install_command(
    fixture.command_for("0.1.0-new", fixture.archive.len()),
    &fixture.archive,
    |_| {},
  )
  .await
  .unwrap();

  assert_eq!(
    std::fs::read_link(base.join("current")).unwrap(),
    std::path::PathBuf::from("versions/0.1.0-new")
  );
  assert_eq!(std::fs::read_dir(old).unwrap().count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn installer_reuses_identical_existing_components_without_replacing_files() {
  use std::os::unix::fs::MetadataExt as _;
  for manifest in [None, Some("matching manifest")] {
    let mut fixture = BundleFixture::new();
    if let Some(manifest) = manifest {
      std::fs::write(fixture.directory.join("source/manifest.json"), manifest).unwrap();
      fixture.rebuild_archive();
    }
    fixture.install("0.1.0-test").await.unwrap();
    let destination = fixture.directory.join("home/.tokn/ctl/versions/0.1.0-test");
    let inodes: Vec<_> = ["ctl-agent", "ctmuxd", "ctl-taskd"]
      .iter()
      .map(|binary| std::fs::metadata(destination.join(binary)).unwrap().ino())
      .collect();
    fixture.install("0.1.0-test").await.unwrap();
    for (binary, inode) in ["ctl-agent", "ctmuxd", "ctl-taskd"].iter().zip(inodes) {
      assert_eq!(
        std::fs::metadata(destination.join(binary)).unwrap().ino(),
        inode
      );
      assert_eq!(
        std::fs::read_to_string(destination.join(binary)).unwrap(),
        *binary
      );
    }
    if let Some(manifest) = manifest {
      assert_eq!(
        std::fs::read_to_string(destination.join("manifest.json")).unwrap(),
        manifest
      );
    }
    fixture.assert_staging_clean();
  }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_rejects_differing_same_id_components_without_activation() {
  use std::os::unix::fs::MetadataExt as _;
  use std::sync::Mutex;
  for binary in ["ctl-agent", "ctmuxd", "ctl-taskd"] {
    let mut fixture = BundleFixture::new();
    fixture.install("0.1.0-test").await.unwrap();
    fixture.install("0.1.0-active").await.unwrap();
    let base = fixture.directory.join("home/.tokn/ctl");
    let destination = base.join("versions/0.1.0-test");
    let inode = std::fs::metadata(destination.join(binary)).unwrap().ino();
    std::fs::write(
      fixture.directory.join("source").join(binary),
      "different build",
    )
    .unwrap();
    fixture.rebuild_archive();
    let events = Mutex::new(Vec::new());
    let result = tokio::time::timeout(
      std::time::Duration::from_secs(10),
      run_install_command(
        fixture.command(fixture.archive.len()),
        &fixture.archive,
        |event| events.lock().unwrap().push(event),
      ),
    )
    .await
    .unwrap();
    assert!(matches!(
      result,
      Err(CoreError::SshCommandFailed { diagnostic, .. })
        if diagnostic.contains("already exists with different") && diagnostic.contains(binary)
    ));
    let events = events.into_inner().unwrap();
    assert!(!events.contains(&RemoteInstallEvent::Activating));
    assert!(!events.contains(&RemoteInstallEvent::Complete));
    assert_eq!(
      std::fs::read_link(base.join("current")).unwrap(),
      std::path::PathBuf::from("versions/0.1.0-active")
    );
    assert_eq!(
      std::fs::metadata(destination.join(binary)).unwrap().ino(),
      inode
    );
    for component in ["ctl-agent", "ctmuxd", "ctl-taskd"] {
      assert_eq!(
        std::fs::read_to_string(destination.join(component)).unwrap(),
        component
      );
    }
    fixture.assert_staging_clean();
  }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_rejects_differing_or_missing_same_id_manifests() {
  for (original, replacement) in [
    (Some("original manifest"), Some("different manifest")),
    (Some("original manifest"), None),
    (None, Some("provided manifest")),
  ] {
    let mut fixture = BundleFixture::new();
    let source = fixture.directory.join("source/manifest.json");
    if let Some(manifest) = original {
      std::fs::write(&source, manifest).unwrap();
      fixture.rebuild_archive();
    }
    fixture.install("0.1.0-test").await.unwrap();
    if let Some(manifest) = replacement {
      std::fs::write(&source, manifest).unwrap();
    } else {
      std::fs::remove_file(&source).unwrap();
    }
    fixture.rebuild_archive();
    assert!(matches!(
      fixture.install("0.1.0-test").await,
      Err(CoreError::SshCommandFailed { diagnostic, .. })
        if diagnostic.contains("already exists with different manifest.json")
    ));
    let base = fixture.directory.join("home/.tokn/ctl");
    assert_eq!(
      std::fs::read_link(base.join("current")).unwrap(),
      std::path::PathBuf::from("versions/0.1.0-test")
    );
    let manifest = base.join("versions/0.1.0-test/manifest.json");
    match original {
      Some(original) => assert_eq!(std::fs::read_to_string(manifest).unwrap(), original),
      None => assert!(!manifest.exists()),
    }
    fixture.assert_staging_clean();
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
  for binary in ["ctl-agent", "ctmuxd", "ctl-taskd"] {
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
