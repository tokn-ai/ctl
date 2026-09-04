use std::fs;
use std::path::PathBuf;

use task_store::{DefinitionScope, discover_project, global_path, project_path};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("task-project-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    Self(fs::canonicalize(root).unwrap())
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn project_discovery_recognizes_task_files_and_git_worktree_files() {
  let fixture = Fixture::new();
  let nested = fixture.0.join("src/nested");
  fs::create_dir_all(&nested).unwrap();
  fs::write(
    fixture.0.join(".git"),
    "gitdir: /some/checkout/.git/worktrees/example",
  )
  .unwrap();
  assert_eq!(discover_project(&nested).unwrap(), Some(fixture.0.clone()));
  fs::remove_file(fixture.0.join(".git")).unwrap();
  fs::create_dir(fixture.0.join(".ctl")).unwrap();
  fs::write(fixture.0.join(".ctl/tasks.json"), "{}").unwrap();
  assert_eq!(discover_project(&nested).unwrap(), Some(fixture.0.clone()));
}

#[test]
fn a_nested_git_root_stops_discovery_of_a_parent_store() {
  let fixture = Fixture::new();
  fs::create_dir(fixture.0.join(".ctl")).unwrap();
  fs::write(fixture.0.join(".ctl/tasks.json"), "{}").unwrap();
  let inner = fixture.0.join("inner");
  fs::create_dir_all(inner.join(".git")).unwrap();
  fs::create_dir(inner.join("src")).unwrap();
  assert_eq!(discover_project(&inner.join("src")).unwrap(), Some(inner));
}

#[test]
fn discovering_a_project_does_not_create_a_definition_file() {
  let fixture = Fixture::new();
  fs::create_dir(fixture.0.join(".git")).unwrap();
  let root = discover_project(&fixture.0).unwrap().unwrap();
  let scope = DefinitionScope::Project { project_root: root };
  assert_eq!(scope.path().unwrap(), fixture.0.join(".ctl/tasks.json"));
  assert!(!fixture.0.join(".ctl").exists());
}

#[test]
fn invalid_project_roots_fail_instead_of_resolving_to_a_global_store() {
  let fixture = Fixture::new();
  fs::write(fixture.0.join("file"), "not a directory").unwrap();
  assert!(project_path(&fixture.0.join("file")).is_err());
  assert!(discover_project(&fixture.0.join("missing")).is_err());
}

#[test]
fn global_scope_and_serialized_project_scope_use_the_shared_paths() {
  assert_eq!(
    DefinitionScope::Global.path().unwrap(),
    global_path().unwrap()
  );
  let fixture = Fixture::new();
  let scope = DefinitionScope::Project {
    project_root: fixture.0.clone(),
  };
  let value = serde_json::to_value(&scope).unwrap();
  assert_eq!(value["kind"], "project");
  assert!(value.get("project_root").is_some());
  assert_eq!(scope.path().unwrap(), project_path(&fixture.0).unwrap());
}
