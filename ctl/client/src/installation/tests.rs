use super::*;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let home = Self(std::env::temp_dir().join(format!("ctlup-settings-{}", uuid::Uuid::new_v4())));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&home.0)
      .unwrap();
    home
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn channel_inspection_is_passive_and_explicit_choices_are_atomic() {
  let home = Home::new();
  assert_eq!(default_channel(&home.0).unwrap(), BundleChannel::default());
  assert!(!home.0.join(".tokn").exists());
  set_channel(&home.0, BundleChannel::Stable).unwrap();
  assert_eq!(default_channel(&home.0).unwrap(), BundleChannel::Stable);
  set_channel(&home.0, BundleChannel::Dev).unwrap();
  assert_eq!(default_channel(&home.0).unwrap(), BundleChannel::Dev);
  assert!(
    !std::fs::read_dir(home.0.join(".tokn/ctl"))
      .unwrap()
      .any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".ctlup-settings-"))
  );
  std::fs::write(
    home.0.join(".tokn/ctl/ctlup.json"),
    br#"{"schema_version":99,"channel":"stable"}"#,
  )
  .unwrap();
  assert!(default_channel(&home.0).is_err());
}

#[test]
fn settings_do_not_follow_account_storage_links() {
  let home = Home::new();
  let outside = Home::new();
  std::os::unix::fs::symlink(&outside.0, home.0.join(".tokn")).unwrap();
  assert!(set_channel(&home.0, BundleChannel::Stable).is_err());
  assert!(default_channel(&home.0).is_err());
  assert!(!outside.0.join("ctl").exists());
}
