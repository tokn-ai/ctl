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
  document.tabs.push(document.sessions[0].reference().into());
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
  document.active_tab = Some(WorkspaceTab::Session {
    host_id: "remote-id".into(),
    session_id: "absent".into(),
  });
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

#[test]
fn migrates_legacy_tabs_without_losing_order_and_preserves_a_backup() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let mut value = serde_json::to_value(WorkspaceSnapshot {
    revision: Some("old".into()),
    document: populated(),
  })
  .unwrap();
  value["document"]["schema_version"] = 1.into();
  for tab in value["document"]["tabs"].as_array_mut().unwrap() {
    tab.as_object_mut().unwrap().remove("kind");
  }
  value["document"]["active_tab"]
    .as_object_mut()
    .unwrap()
    .remove("kind");
  value["document"]
    .as_object_mut()
    .unwrap()
    .remove("task_definitions");
  value["document"]
    .as_object_mut()
    .unwrap()
    .remove("task_references");
  let bytes = serde_json::to_vec(&value).unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  let loaded = fixture.repository().load().unwrap();
  assert_eq!(loaded.document, populated());
  assert_ne!(loaded.revision.as_deref(), Some("old"));
  assert_eq!(
    fs::read(fixture.0.join("workspace-v1.backup.json")).unwrap(),
    bytes
  );
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: loaded.revision,
      document: loaded.document,
    })
    .unwrap();
  assert_eq!(
    fixture.repository().load().unwrap().document.schema_version,
    3
  );
}

#[test]
fn incomplete_task_drafts_round_trip_without_becoming_runnable_definitions() {
  let fixture = Fixture::new();
  let mut document = populated();
  document.sidebar_view = SidebarView::Tasks;
  document.task_drafts.push(TaskDefinitionDraft {
    command_line: Some("cargo run \"unfinished".into()),
    scope: None,
    base_revision: DraftBaseRevision::Unknown,
    definition_id: uuid::Uuid::new_v4().to_string(),
    definition: task_proto::TaskDefinition {
      name: String::new(),
      program: String::new(),
      arguments: vec![String::new()],
      working_directory: Some("unfinished/relative".into()),
      execution_mode: task_proto::ExecutionMode::Background,
    },
  });
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: document.clone(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap().document, document);
  document.task_drafts.push(document.task_drafts[0].clone());
  assert!(document.validate().is_err());
}

fn legacy_definition() -> SavedTaskDefinition {
  SavedTaskDefinition {
    definition_id: uuid::Uuid::new_v4().to_string(),
    revision: "legacy-revision".into(),
    definition: task_proto::TaskDefinition {
      name: "build".into(),
      program: "cargo".into(),
      arguments: vec!["build".into()],
      working_directory: None,
      execution_mode: task_proto::ExecutionMode::Background,
    },
  }
}

fn write_legacy(fixture: &Fixture, saved: &SavedTaskDefinition) -> Vec<u8> {
  fs::create_dir_all(&fixture.0).unwrap();
  let mut document = populated();
  document.schema_version = 2;
  document.task_definitions.push(saved.clone());
  document.task_references.push(TaskReference {
    host_id: "local".into(),
    task_id: uuid::Uuid::new_v4().to_string(),
    definition_id: Some(saved.definition_id.clone()),
    definition_scope: None,
    applied_revision: Some(saved.revision.clone()),
    is_default: true,
  });
  let bytes = serde_json::to_vec(&WorkspaceSnapshot {
    revision: Some("workspace-before-import".into()),
    document,
  })
  .unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  bytes
}

#[test]
fn imports_definitions_once_and_preserves_refs_and_legacy_directory_semantics() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let original = write_legacy(&fixture, &saved);
  let store = task_store::Repository::new(fixture.0.join("tasks.json"));
  // Simulate a crash after import but before the workspace migration commits.
  store.import_legacy(std::slice::from_ref(&saved)).unwrap();
  let snapshot = fixture.repository().load().unwrap();
  assert_eq!(snapshot.document.schema_version, 3);
  assert!(snapshot.document.task_definitions.is_empty());
  let definitions = store.load().unwrap().definitions;
  assert_eq!(definitions.len(), 1);
  assert_eq!(definitions[0].definition, saved.definition);
  assert_eq!(definitions[0].definition_id, saved.definition_id);
  let original: WorkspaceSnapshot = serde_json::from_slice(&original).unwrap();
  assert_eq!(snapshot.document.sessions, original.document.sessions);
  assert_eq!(
    snapshot.document.task_references[0].task_id,
    original.document.task_references[0].task_id
  );
  assert_eq!(
    snapshot.document.task_references[0]
      .applied_revision
      .as_deref(),
    Some(definitions[0].revision.as_str())
  );
  assert!(fixture.0.join("workspace-v2.backup.json").is_file());
  assert_eq!(fixture.repository().load().unwrap(), snapshot);

  // A later CLI change cannot be overwritten by persisting an old workspace view.
  let mut updated = saved.definition;
  updated.arguments = vec!["test".into()];
  store
    .save(
      &saved.definition_id,
      Some(&definitions[0].revision),
      updated.clone(),
    )
    .unwrap();
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: snapshot.revision,
      document: snapshot.document,
    })
    .unwrap();
  assert_eq!(store.load().unwrap().definitions[0].definition, updated);
}

#[test]
fn conflicting_import_keeps_workspace_and_shared_definition_unchanged() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let bytes = write_legacy(&fixture, &saved);
  let store = task_store::Repository::new(fixture.0.join("tasks.json"));
  let mut other = saved.definition.clone();
  other.arguments = vec!["test".into()];
  store
    .save(&saved.definition_id, None, other.clone())
    .unwrap();
  assert!(fixture.repository().load().is_err());
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
  assert_eq!(store.load().unwrap().definitions[0].definition, other);
}

#[test]
fn incomplete_migration_backup_does_not_authorize_replacing_the_workspace() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let original = write_legacy(&fixture, &saved);
  let backup = fixture.0.join("workspace-v2.backup.json");
  fs::write(&backup, b"partial").unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "workspace_backup_conflict"
  );
  assert_eq!(
    fs::read(fixture.0.join("workspace.json")).unwrap(),
    original
  );
  assert_eq!(fs::read(backup).unwrap(), b"partial");
  assert!(!fixture.0.join("tasks.json").exists());
}

#[test]
fn migration_that_exceeds_the_size_limit_preserves_the_readable_source() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let bytes = write_legacy(&fixture, &saved);
  let mut original: WorkspaceSnapshot = serde_json::from_slice(&bytes).unwrap();
  let reference = original.document.task_references[0].clone();
  for _ in 0..14_000 {
    original.document.task_references.push(TaskReference {
      task_id: uuid::Uuid::new_v4().to_string(),
      is_default: false,
      ..reference.clone()
    });
  }
  original.document.validate().unwrap();
  let bytes = serde_json::to_vec(&original).unwrap();
  assert!(bytes.len() < 4 * 1024 * 1024);
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "workspace_too_large"
  );
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
}

#[test]
fn draft_revision_distinguishes_unknown_base_from_new_definition() {
  let mut value = serde_json::json!({
    "definition_id": "draft",
    "definition": legacy_definition().definition,
  });
  let old: TaskDefinitionDraft = serde_json::from_value(value.clone()).unwrap();
  assert_eq!(old.base_revision, DraftBaseRevision::Unknown);
  value["base_revision"] = serde_json::Value::Null;
  let new: TaskDefinitionDraft = serde_json::from_value(value.clone()).unwrap();
  assert_eq!(new.base_revision, DraftBaseRevision::New);
  assert!(serde_json::to_value(new).unwrap()["base_revision"].is_null());
  value["base_revision"] = "saved-revision".into();
  let edited: TaskDefinitionDraft = serde_json::from_value(value).unwrap();
  assert_eq!(
    edited.base_revision,
    DraftBaseRevision::Saved("saved-revision".into())
  );
}
