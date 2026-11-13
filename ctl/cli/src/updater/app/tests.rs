use super::*;
use crate::updater::arguments::AppArguments;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt as _;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-local-app-{}", uuid::Uuid::new_v4()));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
      let mut builder = builder;
      builder.mode(0o700);
      builder
    };
    builder.create(&path).unwrap();
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
  ctl_client::desktop_storage::set_channel(&home.0, BundleChannel::Stable).unwrap();
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
  #[cfg(windows)]
  let app = directory.join("ctmux");
  assert!(app.exists());
  #[cfg(target_os = "macos")]
  let changed = app.join("Contents/Resources/local-build-fixture");
  #[cfg(target_os = "macos")]
  fs::write(&changed, b"extra local file").unwrap();
  #[cfg(windows)]
  let changed = app.join("extra-local-file");
  #[cfg(windows)]
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
  #[cfg(any(target_os = "macos", windows))]
  assert!(!changed.exists());
  assert_eq!(
    ctl_client::desktop_storage::default_channel(&home.0).unwrap(),
    BundleChannel::Stable
  );
  assert!(!home.0.join(".tokn/ctl/current").exists());
}

#[cfg(windows)]
#[test]
#[ignore = "requires CTLUP_TEST_DESKTOP_ARCHIVE pointing at a native Windows portable ZIP"]
fn prebuilt_windows_archive_install_update_and_repair() {
  let archive = PathBuf::from(
    std::env::var_os("CTLUP_TEST_DESKTOP_ARCHIVE").expect("native portable archive fixture"),
  );
  let home = Home::new();
  ctl_client::desktop_storage::set_channel(&home.0, BundleChannel::Stable).unwrap();
  let destination = app_directory(&home.0, true).unwrap().join("ctmux");
  let target = DesktopTarget::native().unwrap();
  let mut version = String::new();
  // This caller-provided package still passes the production target, PE and
  // inventory checks; the archive manifest supplies only its product version.
  {
    use std::io::Read as _;
    let mut zip = zip::ZipArchive::new(fs::File::open(&archive).unwrap()).unwrap();
    zip
      .by_name("ctmux/portable.json")
      .unwrap()
      .take(4 * 1024 * 1024)
      .read_to_string(&mut version)
      .unwrap();
  }
  let metadata: serde_json::Value = serde_json::from_str(&version).unwrap();
  let version = metadata["app_version"].as_str().unwrap();
  for iteration in 0..3 {
    let mut work = activation::Workspace::begin(&destination).unwrap();
    let installed = activation::existing(&destination).unwrap();
    let payload = portable::stage(&archive, work.staging(), version, target.triple()).unwrap();
    if iteration == 1 {
      assert!(portable::matches(&destination, &payload).unwrap());
      fs::write(destination.join("repair-fixture"), b"changed").unwrap();
    }
    if iteration == 2 {
      fs::write(destination.join("ctld.exe"), b"damaged").unwrap();
    }
    let previous = work.activate(&payload, installed.as_ref()).unwrap();
    portable::verify(&destination, version, target.triple()).unwrap();
    assert_eq!(previous.is_some(), iteration > 0);
    assert!(!destination.join("repair-fixture").exists());
  }
  assert_eq!(
    ctl_client::desktop_storage::default_channel(&home.0).unwrap(),
    BundleChannel::Stable
  );
  assert!(!home.0.join(".tokn/ctl/current").exists());
}
