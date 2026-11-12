use super::*;
use crate::updater::arguments::AppArguments;
use std::fs;
use std::os::unix::fs::DirBuilderExt as _;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-local-app-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
#[ignore = "builds the full desktop from the current checkout with native dependencies"]
async fn local_source_install_and_repair_preserve_the_saved_channel() {
  let home = Home::new();
  ctl_client::installation::set_channel(&home.0, BundleChannel::Stable).unwrap();
  let options = || AppArguments {
    local: true,
    channel: None,
  };
  run(&home.0, AppCommand::Install(options())).await.unwrap();
  let directory = app_directory(&home.0, false).unwrap();
  #[cfg(target_os = "macos")]
  let app = directory.join("ctmux.app");
  #[cfg(target_os = "linux")]
  let app = directory.join("ctmux.AppImage");
  assert!(app.exists());
  #[cfg(target_os = "macos")]
  let changed = app.join("Contents/Resources/local-build-fixture");
  #[cfg(target_os = "macos")]
  fs::write(&changed, b"extra local file").unwrap();
  #[cfg(target_os = "linux")]
  {
    use std::io::Write as _;
    fs::OpenOptions::new()
      .append(true)
      .open(&app)
      .unwrap()
      .write_all(b"extra local bytes")
      .unwrap();
  }
  run(&home.0, AppCommand::Update(options())).await.unwrap();
  #[cfg(target_os = "macos")]
  assert!(!changed.exists());
  assert_eq!(
    ctl_client::installation::default_channel(&home.0).unwrap(),
    BundleChannel::Stable
  );
  assert!(!home.0.join(".tokn/ctl/current").exists());
}
