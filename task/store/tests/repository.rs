use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use task_proto::{ExecutionMode, TaskDefinition};
use task_store::{Repository, SavedTaskDefinition, StoreError};

struct Fixture {
  root: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("task-store-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    Self {
      root: fs::canonicalize(root).unwrap(),
    }
  }

  fn path(&self) -> PathBuf {
    self.root.join("config/tasks.json")
  }

  fn repository(&self) -> Repository {
    Repository::new(self.path())
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.root);
  }
}

fn definition(name: &str) -> TaskDefinition {
  TaskDefinition {
    name: name.into(),
    program: "cargo".into(),
    arguments: vec!["build".into()],
    working_directory: None,
    execution_mode: ExecutionMode::Background,
  }
}

fn legacy(definition_id: &str, name: &str) -> SavedTaskDefinition {
  SavedTaskDefinition {
    definition_id: definition_id.into(),
    revision: "legacy-revision".into(),
    definition: definition(name),
  }
}

#[test]
fn reading_an_absent_store_has_no_filesystem_side_effects() {
  let fixture = Fixture::new();
  assert!(fixture.repository().load().unwrap().definitions.is_empty());
  assert!(!fixture.root.join("config").exists());
}

#[test]
fn saved_definitions_round_trip_without_resolving_legacy_directories() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let mut relative = definition("relative");
  relative.working_directory = Some("../project".into());
  let first = repository.save("relative-id", None, relative).unwrap();
  let second = repository
    .save("home-id", None, definition("home"))
    .unwrap();
  assert!(first.revision.starts_with("sha256:"));
  assert_eq!(repository.load().unwrap().definitions, vec![second, first]);
}

#[test]
fn saves_are_create_only_or_revision_checked_and_removal_is_checked() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let first = repository
    .save("build-id", None, definition("build"))
    .unwrap();
  assert!(matches!(
    repository.save("build-id", None, definition("build")),
    Err(StoreError::Conflict { .. })
  ));
  let mut changed = first.definition.clone();
  changed.arguments.push("--release".into());
  let second = repository
    .save("build-id", Some(&first.revision), changed)
    .unwrap();
  assert_ne!(second.revision, first.revision);
  assert!(matches!(
    repository.remove("build-id", &first.revision),
    Err(StoreError::Conflict { .. })
  ));
  assert!(matches!(
    repository.save("build-id", Some(&first.revision), first.definition),
    Err(StoreError::Conflict { .. })
  ));
  repository.remove("build-id", &second.revision).unwrap();
  assert!(repository.load().unwrap().definitions.is_empty());
}

#[test]
fn manual_definition_edits_invalidate_a_stored_revision_without_trusting_its_uuid() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save("build-id", None, definition("build"))
    .unwrap();
  let mut document: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
  document["definitions"][0]["definition"]["arguments"] = serde_json::json!(["test"]);
  document["definitions"][0]["revision"] = "unchanged-user-written-revision".into();
  fs::write(
    fixture.path(),
    serde_json::to_vec_pretty(&document).unwrap(),
  )
  .unwrap();
  let edited = repository.load().unwrap().definitions.remove(0);
  assert_ne!(saved.revision, edited.revision);
  assert!(edited.revision.starts_with("sha256:"));
  assert!(matches!(
    repository.save("build-id", Some(&saved.revision), saved.definition),
    Err(StoreError::Conflict { .. })
  ));
  let before = fs::read(fixture.path()).unwrap();
  assert_eq!(
    repository
      .save(
        "build-id",
        Some(&edited.revision),
        edited.definition.clone()
      )
      .unwrap(),
    edited
  );
  assert_eq!(fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn concurrent_edits_to_one_definition_have_one_winner() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let original = repository
    .save("build-id", None, definition("build"))
    .unwrap();
  let barrier = Arc::new(Barrier::new(2));
  let workers: Vec<_> = ["test", "check"]
    .into_iter()
    .map(|argument| {
      let repository = repository.clone();
      let original = original.clone();
      let barrier = Arc::clone(&barrier);
      std::thread::spawn(move || {
        let mut changed = original.definition;
        changed.arguments = vec![argument.into()];
        barrier.wait();
        repository.save("build-id", Some(&original.revision), changed)
      })
    })
    .collect();
  let results: Vec<_> = workers
    .into_iter()
    .map(|worker| worker.join().unwrap())
    .collect();
  assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
  assert_eq!(
    results
      .iter()
      .filter(|result| matches!(result, Err(StoreError::Conflict { .. })))
      .count(),
    1
  );
  assert_eq!(repository.load().unwrap().definitions.len(), 1);
}

#[test]
fn concurrent_creates_preserve_both_definitions() {
  let fixture = Fixture::new();
  let barrier = Arc::new(Barrier::new(2));
  let workers: Vec<_> = ["build", "check"]
    .into_iter()
    .map(|name| {
      let repository = fixture.repository();
      let barrier = Arc::clone(&barrier);
      std::thread::spawn(move || {
        barrier.wait();
        repository.save(name, None, definition(name))
      })
    })
    .collect();
  for worker in workers {
    worker.join().unwrap().unwrap();
  }
  assert_eq!(fixture.repository().load().unwrap().definitions.len(), 2);
}

#[test]
fn import_is_idempotent_preserves_ids_and_commits_all_or_nothing() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let imported = repository
    .import_legacy(&[legacy("build-id", "build")])
    .unwrap();
  assert_eq!(imported.definitions[0].definition_id, "build-id");
  assert_ne!(imported.definitions[0].revision, "legacy-revision");
  let before = fs::read(fixture.path()).unwrap();
  assert_eq!(
    repository
      .import_legacy(&[legacy("build-id", "build")])
      .unwrap(),
    imported
  );
  assert_eq!(fs::read(fixture.path()).unwrap(), before);

  let mut conflict = legacy("build-id", "build");
  conflict.definition.arguments.push("--release".into());
  assert!(matches!(
    repository.import_legacy(&[legacy("new-id", "new"), conflict]),
    Err(StoreError::Conflict { .. })
  ));
  assert_eq!(fs::read(fixture.path()).unwrap(), before);
  assert!(matches!(
    repository.import_legacy(&[legacy("new-id", "new"), legacy("other-id", "build")]),
    Err(StoreError::NameConflict { .. })
  ));
  assert_eq!(fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn scoped_names_are_unique_and_renaming_preserves_identity() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save("build-id", None, definition("build"))
    .unwrap();
  assert!(matches!(
    repository.save("other-id", None, definition("build")),
    Err(StoreError::NameConflict { .. })
  ));
  let renamed = repository
    .save("build-id", Some(&saved.revision), definition("compile"))
    .unwrap();
  assert_eq!(renamed.definition_id, saved.definition_id);
  repository
    .save("other-id", None, definition("build"))
    .unwrap();
  assert_eq!(repository.load().unwrap().definitions.len(), 2);
}

#[test]
fn corrupt_unsupported_and_oversized_files_are_never_replaced() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
  for bytes in [
    b"broken JSON".to_vec(),
    br#"{"schema_version":999,"definitions":[]}"#.to_vec(),
    br#"{"schema_version":1,"definitions":[],"unknown_field":true}"#.to_vec(),
    vec![b' '; 4 * 1024 * 1024 + 1],
  ] {
    fs::write(fixture.path(), &bytes).unwrap();
    let repository = fixture.repository();
    assert!(repository.load().is_err());
    assert!(
      repository
        .save("build-id", None, definition("build"))
        .is_err()
    );
    assert!(
      repository
        .import_legacy(&[legacy("build-id", "build")])
        .is_err()
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
  }
}

#[test]
fn unknown_nested_definition_fields_are_preserved_when_save_or_import_is_rejected() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save("build-id", None, definition("build"))
    .unwrap();
  let mut document: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
  document["definitions"][0]["definition"]["environment"] = serde_json::json!({"PROJECT": "keep"});
  let bytes = serde_json::to_vec_pretty(&document).unwrap();
  fs::write(fixture.path(), &bytes).unwrap();
  assert!(matches!(
    repository.load(),
    Err(StoreError::InvalidDefinition { .. })
  ));
  assert!(matches!(
    repository.save("build-id", Some(&saved.revision), definition("compile")),
    Err(StoreError::InvalidDefinition { .. })
  ));
  assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
  assert!(matches!(
    repository.import_legacy(&[legacy("new-id", "new")]),
    Err(StoreError::InvalidDefinition { .. })
  ));
  assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn symlink_store_and_lock_paths_are_rejected_without_touching_the_target() {
  use std::os::unix::fs::symlink;

  let fixture = Fixture::new();
  fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
  let unrelated = fixture.root.join("unrelated");
  fs::write(&unrelated, b"keep this").unwrap();
  symlink(&unrelated, fixture.path()).unwrap();
  assert!(matches!(
    fixture.repository().load(),
    Err(StoreError::UnsafePath { .. })
  ));
  assert!(matches!(
    fixture
      .repository()
      .save("build-id", None, definition("build")),
    Err(StoreError::UnsafePath { .. })
  ));
  fs::remove_file(fixture.path()).unwrap();
  fs::remove_file(fixture.path().with_file_name(".tasks.json.lock")).unwrap();
  symlink(
    &unrelated,
    fixture.path().with_file_name(".tasks.json.lock"),
  )
  .unwrap();
  assert!(matches!(
    fixture
      .repository()
      .save("build-id", None, definition("build")),
    Err(StoreError::UnsafePath { .. })
  ));
  assert_eq!(fs::read(unrelated).unwrap(), b"keep this");
}

#[cfg(unix)]
#[test]
fn created_configuration_and_files_are_private_and_no_temporary_file_remains() {
  use std::os::unix::fs::PermissionsExt;

  let fixture = Fixture::new();
  fixture
    .repository()
    .save("build-id", None, definition("build"))
    .unwrap();
  let parent = fixture.path().parent().unwrap().to_path_buf();
  assert_eq!(
    fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
    0o700
  );
  assert_eq!(
    fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
    0o600
  );
  let entries: Vec<_> = fs::read_dir(parent)
    .unwrap()
    .map(|entry| entry.unwrap().file_name())
    .collect();
  assert_eq!(entries.len(), 2);
  assert!(
    entries
      .iter()
      .all(|name| !name.to_string_lossy().ends_with(".tmp"))
  );
}
