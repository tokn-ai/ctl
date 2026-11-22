use super::*;
use std::fs;
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-storage-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn desktop_public_directory_does_not_create_updater_state() {
  let home = Home::new();
  assert_eq!(
    app_directory(&home.0, false).unwrap(),
    home.0.join(".tokn/ctl/apps")
  );
  assert!(!home.0.join(".tokn").exists());
  app_directory(&home.0, true).unwrap();
  assert!(!home.0.join(".tokn/ctl/ctlup").exists());
}

#[test]
fn rejects_file_and_directory_aliases_in_storage() {
  let home = Home::new();
  fs::write(home.0.join(".tokn"), b"file").unwrap();
  assert!(app_directory(&home.0, true).is_err());
  fs::remove_file(home.0.join(".tokn")).unwrap();
  let directory = app_directory(&home.0, true).unwrap();
  fs::remove_dir(&directory).unwrap();
  fs::write(directory, b"file").unwrap();
  assert!(app_directory(&home.0, true).is_err());
}
