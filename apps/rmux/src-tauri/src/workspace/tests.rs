use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::repository::Repository;
use super::*;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("rmux-workspace-test-{}", uuid::Uuid::new_v4())))
  }

  fn repository(&self) -> Repository {
    Repository::new(self.0.clone())
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

fn populated() -> WorkspaceDocument {
  let mut document = WorkspaceDocument::default();
  document.hosts.push(WorkspaceHost {
    host_id: "remote-id".into(),
    target: ConnectionTargetDto::ssh("test"),
  });
  document.sessions.push(WorkspaceSession {
    host_id: "remote-id".into(),
    session_id: "session-id".into(),
    name: "shell".into(),
    last_known_cwd: Some("/work".into()),
    last_known_cwd_display: Some("~/work".into()),
  });
  document.tabs.push(document.sessions[0].reference());
  document.active_tab = document.tabs.first().cloned();
  document
}

#[test]
fn missing_workspace_loads_empty_and_round_trips_only_metadata() {
  let fixture = Fixture::new();
  let initial = fixture.repository().load().unwrap();
  assert_eq!(initial, WorkspaceSnapshot::default());
  assert!(!fixture.0.join("workspace.json").exists());
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), saved);
  let bytes = fs::read_to_string(fixture.0.join("workspace.json")).unwrap();
  for runtime_field in [
    "password",
    "attachment_token",
    "next_sequence",
    "running_command",
    "terminal_size",
  ] {
    assert!(!bytes.contains(runtime_field));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
      fs::metadata(fixture.0.join("workspace.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }
}

#[test]
fn corrupt_and_future_workspaces_are_never_overwritten() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let mut future = WorkspaceSnapshot {
    revision: Some("future".into()),
    document: populated(),
  };
  future.document.schema_version = 99;
  for bytes in [
    b"broken json".to_vec(),
    serde_json::to_vec(&future).unwrap(),
  ] {
    fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
    assert!(fixture.repository().load().is_err());
    assert!(
      fixture
        .repository()
        .update(UpdateWorkspaceRequest {
          expected_revision: None,
          document: populated()
        })
        .is_err()
    );
    assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
  }
}

#[test]
fn stale_and_concurrent_writers_cannot_lose_updates() {
  let fixture = Fixture::new();
  let gate = Arc::new(Barrier::new(3));
  let threads: Vec<_> = (0..2)
    .map(|_| {
      let repository = fixture.repository();
      let gate = Arc::clone(&gate);
      std::thread::spawn(move || {
        gate.wait();
        repository.update(UpdateWorkspaceRequest {
          expected_revision: None,
          document: populated(),
        })
      })
    })
    .collect();
  gate.wait();
  let results: Vec<_> = threads
    .into_iter()
    .map(|thread| thread.join().unwrap())
    .collect();
  assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
  assert_eq!(
    results
      .iter()
      .find_map(|result| result.as_ref().err())
      .unwrap()
      .code,
    "workspace_conflict"
  );
  let saved = fixture.repository().load().unwrap();
  assert_eq!(saved.document, populated());
}

#[test]
fn validates_membership_tabs_and_host_identity() {
  let mut document = populated();
  document.sessions[0].host_id = "absent".into();
  assert!(document.validate().is_err());
  document = populated();
  document.tabs.push(document.tabs[0].clone());
  assert!(document.validate().is_err());
  document = populated();
  document.active_tab.as_mut().unwrap().session_id = "absent".into();
  assert!(document.validate().is_err());
  document = populated();
  document.hosts.push(document.hosts[1].clone());
  assert!(document.validate().is_err());
}

#[test]
fn failed_write_preserves_previous_document() {
  let fixture = Fixture::new();
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated(),
    })
    .unwrap();
  let mut invalid = populated();
  invalid.sessions[0].last_known_cwd = Some("x".repeat(5000));
  assert!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: saved.revision.clone(),
        document: invalid
      })
      .is_err()
  );
  assert_eq!(fixture.repository().load().unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn refuses_symlinks_without_modifying_their_target() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let target = fixture.0.join("original");
  fs::write(&target, "preserve").unwrap();
  std::os::unix::fs::symlink(&target, fixture.0.join("workspace.json")).unwrap();
  assert!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: None,
        document: populated()
      })
      .is_err()
  );
  assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
}
