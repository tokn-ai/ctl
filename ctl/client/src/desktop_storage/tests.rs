use super::*;
use ctl_core::component_update::BundleChannel;
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
fn portable_storage_and_channel_round_trip_without_runtime_installation() {
  let home = Home::new();
  assert_eq!(default_channel(&home.0).unwrap(), BundleChannel::default());
  assert!(!home.0.join(".tokn").exists());
  assert_eq!(
    app_directory(&home.0, false).unwrap(),
    home.0.join(".tokn/ctl/apps")
  );
  assert!(!home.0.join(".tokn").exists());
  app_directory(&home.0, true).unwrap();
  for channel in [
    BundleChannel::Stable,
    BundleChannel::Nightly,
    BundleChannel::Stable,
  ] {
    set_channel(&home.0, channel).unwrap();
    assert_eq!(default_channel(&home.0).unwrap(), channel);
  }
  assert!(!home.0.join(".tokn/ctl/current").exists());
}

#[test]
fn invalid_settings_are_reported_without_replacing_them() {
  let home = Home::new();
  app_directory(&home.0, true).unwrap();
  let path = home.0.join(".tokn/ctl/ctlup.json");
  for bytes in [
    br#"{"schema_version":2,"channel":"stable"}"#.to_vec(),
    vec![b'x'; 4097],
  ] {
    fs::write(&path, &bytes).unwrap();
    assert!(default_channel(&home.0).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes);
  }
}

#[test]
fn rejects_file_and_directory_aliases_in_storage() {
  let home = Home::new();
  fs::write(home.0.join(".tokn"), b"file").unwrap();
  assert!(app_directory(&home.0, true).is_err());
  fs::remove_file(home.0.join(".tokn")).unwrap();
  app_directory(&home.0, true).unwrap();
  fs::create_dir(home.0.join(".tokn/ctl/ctlup.json")).unwrap();
  assert!(default_channel(&home.0).is_err());
  assert!(set_channel(&home.0, BundleChannel::Stable).is_err());
}
