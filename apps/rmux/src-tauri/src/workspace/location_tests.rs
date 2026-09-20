use std::fs;
use std::path::PathBuf;

use super::repository::Repository;
use super::{
  SidebarView, UpdateWorkspaceRequest, WorkspaceDocument, WorkspaceSession, WorkspaceSnapshot,
};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!(
      "rmux-workspace-location-test-{}",
      uuid::Uuid::new_v4()
    )))
  }

  fn destination(&self) -> PathBuf {
    self.0.join("current")
  }

  fn legacy(&self) -> PathBuf {
    self.0.join("legacy")
  }

  fn repository(&self) -> Repository {
    Repository::new(self.destination()).with_legacy_directory(self.legacy())
  }

  fn write_legacy(&self, snapshot: &WorkspaceSnapshot) -> Vec<u8> {
    fs::create_dir_all(self.legacy()).unwrap();
    // Deliberately retain formatting that serializing the document would change.
    let mut bytes = serde_json::to_vec(snapshot).unwrap();
    bytes.extend_from_slice(b"\n\n");
    fs::write(self.legacy().join("workspace.json"), &bytes).unwrap();
    bytes
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

fn populated(revision: &str) -> WorkspaceSnapshot {
  let mut document = WorkspaceDocument::default();
  document.sessions.push(WorkspaceSession {
    host_id: "local".into(),
    session_id: "remembered-session".into(),
    name: "shell".into(),
    last_known_cwd: Some("/work".into()),
    last_known_cwd_display: Some("~/work".into()),
  });
  document.tabs.push(document.sessions[0].reference().into());
  document.active_tab = document.tabs.first().cloned();
  WorkspaceSnapshot {
    revision: Some(revision.into()),
    document,
  }
}

#[test]
fn relocates_current_workspace_without_changing_bytes_revision_or_legacy_files() {
  let fixture = Fixture::new();
  let previous = populated("legacy-revision");
  let bytes = fixture.write_legacy(&previous);
  let old_backup = fixture.legacy().join("workspace-v5.backup.json");
  fs::write(&old_backup, b"older backup").unwrap();

  assert_eq!(fixture.repository().load().unwrap(), previous);
  assert_eq!(
    fs::read(fixture.destination().join("workspace.json")).unwrap(),
    bytes
  );
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    bytes
  );
  assert_eq!(fs::read(&old_backup).unwrap(), b"older backup");
  assert!(
    !fixture
      .destination()
      .join("workspace-v5.backup.json")
      .exists()
  );

  let mut changed = previous.document;
  changed.sidebar_view = SidebarView::Tasks;
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: previous.revision,
      document: changed.clone(),
    })
    .unwrap();

  assert_eq!(saved.document, changed);
  assert_ne!(saved.revision.as_deref(), Some("legacy-revision"));
  assert_eq!(fixture.repository().load().unwrap(), saved);
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    bytes
  );
}

#[test]
fn first_update_uses_the_existing_legacy_revision() {
  let fixture = Fixture::new();
  let previous = populated("legacy-revision");
  let bytes = fixture.write_legacy(&previous);
  let mut changed = previous.document;
  changed.sessions[0].name = "renamed".into();

  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: previous.revision,
      document: changed.clone(),
    })
    .unwrap();

  assert_eq!(saved.document, changed);
  assert_eq!(fixture.repository().load().unwrap(), saved);
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    bytes
  );
}

#[test]
fn current_workspace_takes_precedence_over_unreadable_legacy_workspace() {
  let fixture = Fixture::new();
  let existing = Repository::new(fixture.destination())
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated("unused").document,
    })
    .unwrap();
  fs::create_dir_all(fixture.legacy()).unwrap();
  fs::write(fixture.legacy().join("workspace.json"), b"broken json").unwrap();

  assert_eq!(fixture.repository().load().unwrap(), existing);
  assert!(!fixture.legacy().join("workspace.lock").exists());
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    b"broken json"
  );
}

#[test]
fn corrupt_current_workspace_is_not_replaced_by_valid_legacy_workspace() {
  let fixture = Fixture::new();
  let bytes = fixture.write_legacy(&populated("legacy-revision"));
  fs::create_dir_all(fixture.destination()).unwrap();
  fs::write(fixture.destination().join("workspace.json"), b"broken json").unwrap();

  assert!(fixture.repository().load().is_err());
  assert_eq!(
    fs::read(fixture.destination().join("workspace.json")).unwrap(),
    b"broken json"
  );
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    bytes
  );
}

#[test]
fn fresh_workspace_does_not_create_the_legacy_directory() {
  let fixture = Fixture::new();

  assert_eq!(
    fixture.repository().load().unwrap(),
    WorkspaceSnapshot::default()
  );
  assert!(!fixture.legacy().exists());
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated("unused").document,
    })
    .unwrap();

  assert!(fixture.destination().join("workspace.json").is_file());
  assert!(!fixture.legacy().exists());
}

#[test]
fn corrupt_unsupported_and_invalid_legacy_workspaces_are_not_copied() {
  let mut future = populated("future");
  future.document.schema_version = 99;
  let mut invalid = populated("invalid");
  invalid.document.sessions[0].host_id = "missing-host".into();
  let mut missing_revision = populated("unused");
  missing_revision.revision = None;
  for bytes in [
    b"broken json".to_vec(),
    serde_json::to_vec(&future).unwrap(),
    serde_json::to_vec(&invalid).unwrap(),
    serde_json::to_vec(&missing_revision).unwrap(),
  ] {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.legacy()).unwrap();
    fs::write(fixture.legacy().join("workspace.json"), &bytes).unwrap();

    assert!(fixture.repository().load().is_err());
    assert!(
      fixture
        .repository()
        .update(UpdateWorkspaceRequest {
          expected_revision: None,
          document: WorkspaceDocument::default(),
        })
        .is_err()
    );
    assert!(!fixture.destination().join("workspace.json").exists());
    assert_eq!(
      fs::read(fixture.legacy().join("workspace.json")).unwrap(),
      bytes
    );
  }
}

#[test]
fn schema_migration_creates_its_backup_in_the_new_directory() {
  let fixture = Fixture::new();
  let mut previous = populated("before-gateway-routes");
  previous.document.schema_version = 5;
  let bytes = fixture.write_legacy(&previous);
  fs::write(
    fixture.legacy().join("workspace-v5.backup.json"),
    b"older backup",
  )
  .unwrap();

  let loaded = fixture.repository().load().unwrap();

  assert_eq!(loaded.document.schema_version, 6);
  assert_eq!(loaded.document.sessions, previous.document.sessions);
  assert_ne!(loaded.revision, previous.revision);
  assert_eq!(
    fs::read(fixture.destination().join("workspace-v5.backup.json")).unwrap(),
    bytes
  );
  assert_eq!(
    fs::read(fixture.legacy().join("workspace.json")).unwrap(),
    bytes
  );
  assert_eq!(
    fs::read(fixture.legacy().join("workspace-v5.backup.json")).unwrap(),
    b"older backup"
  );
}

#[test]
fn relocation_waits_for_the_legacy_workspace_lock_before_reading() {
  use std::sync::mpsc;
  use std::time::Duration;

  let fixture = Fixture::new();
  fixture.write_legacy(&populated("before-old-app-save"));
  let lock = fs::File::create(fixture.legacy().join("workspace.lock")).unwrap();
  lock.lock().unwrap();
  let repository = fixture.repository();
  let (started_sender, started_receiver) = mpsc::channel();
  let (result_sender, result_receiver) = mpsc::channel();
  let worker = std::thread::spawn(move || {
    started_sender.send(()).unwrap();
    result_sender.send(repository.load()).unwrap();
  });
  started_receiver
    .recv_timeout(Duration::from_secs(10))
    .unwrap();
  let while_locked = result_receiver.recv_timeout(Duration::from_millis(150));
  let latest = populated("after-old-app-save");
  let bytes = fixture.write_legacy(&latest);
  drop(lock);
  let loaded = result_receiver.recv_timeout(Duration::from_secs(10));
  worker.join().unwrap();

  assert!(matches!(while_locked, Err(mpsc::RecvTimeoutError::Timeout)));
  assert_eq!(loaded.unwrap().unwrap(), latest);
  assert_eq!(
    fs::read(fixture.destination().join("workspace.json")).unwrap(),
    bytes
  );
}

#[cfg(unix)]
#[test]
fn refuses_a_symlinked_legacy_workspace_without_copying_its_target() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.legacy()).unwrap();
  let target = fixture.0.join("original.json");
  let bytes = serde_json::to_vec(&populated("original")).unwrap();
  fs::write(&target, &bytes).unwrap();
  std::os::unix::fs::symlink(&target, fixture.legacy().join("workspace.json")).unwrap();

  assert!(fixture.repository().load().is_err());
  assert!(!fixture.destination().join("workspace.json").exists());
  assert_eq!(fs::read(target).unwrap(), bytes);
  assert!(
    fs::symlink_metadata(fixture.legacy().join("workspace.json"))
      .unwrap()
      .file_type()
      .is_symlink()
  );
}
