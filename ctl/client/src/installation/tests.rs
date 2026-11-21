use super::*;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let home = Self(std::env::temp_dir().join(format!("ctlup-settings-{}", uuid::Uuid::new_v4())));
    private_fs::create_directory(&home.0).unwrap();
    home
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn runtime_source_inspection_is_passive_on_a_fresh_profile() {
  let home = Home::new();
  let source = runtime_source(&home.0).unwrap();
  assert!(source.active.is_none());
  assert!(source.origin.is_none());
  assert!(!source.missing);
  assert_eq!(default_channel(&home.0).unwrap(), BundleChannel::default());
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn desktop_storage_is_passive_until_requested_and_rejects_links() {
  let home = Home::new();
  let path = home.0.join(".tokn/ctl/apps");
  assert_eq!(app_directory(&home.0, false).unwrap(), path);
  assert!(!home.0.join(".tokn").exists());
  assert_eq!(app_directory(&home.0, true).unwrap(), path);
  assert!(path.is_dir());

  std::fs::remove_dir(&path).unwrap();
  std::fs::write(&path, b"file").unwrap();
  assert!(app_directory(&home.0, false).is_err());
  assert!(app_directory(&home.0, true).is_err());
  #[cfg(unix)]
  {
    std::fs::remove_file(&path).unwrap();
    let outside = Home::new();
    std::os::unix::fs::symlink(&outside.0, &path).unwrap();
    assert!(app_directory(&home.0, false).is_err());
    assert!(app_directory(&home.0, true).is_err());
  }
}
