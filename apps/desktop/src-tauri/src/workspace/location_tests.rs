use std::fs;
use std::path::PathBuf;

use super::repository::Repository;
use super::{SidebarView, UpdateWorkspaceRequest, WorkspaceDocument, WorkspaceSnapshot};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!(
      "ctmux-workspace-location-test-{}",
      uuid::Uuid::new_v4()
    )))
  }

  fn directory(&self) -> PathBuf {
    self.0.join(".tokn/ctl")
  }

  fn former_directory(&self) -> PathBuf {
    self.0.join(".tokn/ctmux")
  }

  fn repository(&self) -> Repository {
    Repository::new(self.directory())
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn load_and_save_ignore_former_workspace_files_and_their_locks() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.former_directory()).unwrap();
  let former = Repository::new(fixture.former_directory())
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: WorkspaceDocument {
        sidebar_view: SidebarView::Tasks,
        ..WorkspaceDocument::default()
      },
    })
    .unwrap();
  let original = fs::read(fixture.former_directory().join("workspace.json")).unwrap();
  let lock = fs::File::open(fixture.former_directory().join("workspace.lock")).unwrap();
  lock.lock().unwrap();

  assert_eq!(
    fixture.repository().load().unwrap(),
    WorkspaceSnapshot::default()
  );
  assert!(!fixture.directory().join("workspace.json").exists());
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: WorkspaceDocument::default(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), saved);
  assert_ne!(saved, former);
  assert_eq!(
    fs::read(fixture.former_directory().join("workspace.json")).unwrap(),
    original
  );
}

#[test]
fn an_unreadable_former_workspace_does_not_block_a_fresh_workspace() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.former_directory()).unwrap();
  fs::write(
    fixture.former_directory().join("workspace.json"),
    b"broken json",
  )
  .unwrap();

  assert_eq!(
    fixture.repository().load().unwrap(),
    WorkspaceSnapshot::default()
  );
  assert!(!fixture.former_directory().join("workspace.lock").exists());
  assert_eq!(
    fs::read(fixture.former_directory().join("workspace.json")).unwrap(),
    b"broken json"
  );
}
