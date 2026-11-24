use super::*;
use std::fs;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-local-app-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path).unwrap();
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
async fn local_source_install_pins_a_checkout_and_repairs_from_its_archive() {
  let home = Home::new();
  let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
  install_development(&home.0, &checkout, false)
    .await
    .unwrap();
  let store = Installations::new(&home.0);
  let selection = store.desktop_selection().unwrap().unwrap();
  assert_eq!(selection.source, DesktopSource::Checkout);
  let object = store.desktop_object_path(&selection.content_id).unwrap();
  let name = object.file_name().unwrap().to_str().unwrap();
  assert!(name.starts_with("dev-") && name.ends_with("-local"));
  assert!(object.join(payload_name()).exists());
  fs::remove_dir_all(&object).unwrap();
  update(&home.0, false, false).await.unwrap();
  assert_eq!(store.desktop_selection().unwrap(), Some(selection));
  assert!(object.join(payload_name()).exists());
  assert!(
    !app_status(&home.0, store.desktop_selection().unwrap())
      .unwrap()
      .missing
  );
}

#[cfg(windows)]
#[tokio::test]
#[ignore = "requires CTLUP_TEST_DESKTOP_ARCHIVE pointing at a native Windows portable ZIP"]
async fn prebuilt_windows_archive_install_update_and_repair() {
  let archive = PathBuf::from(
    std::env::var_os("CTLUP_TEST_DESKTOP_ARCHIVE").expect("native portable archive fixture"),
  );
  verify_install_update_and_repair(&archive, false).await;
}

#[tokio::test]
async fn managed_desktop_launch_entry_and_exact_repair_use_the_registry() {
  let source = Home::new();
  verify_install_update_and_repair(&fixture_archive(&source.0), true).await;
}

#[tokio::test]
async fn legacy_desktop_launch_entry_and_exact_repair_keep_the_digest_path() {
  let source = Home::new();
  verify_install_update_and_repair(&fixture_archive(&source.0), false).await;
}

fn fixture_archive(directory: &Path) -> PathBuf {
  #[cfg(windows)]
  {
    let target = DesktopTarget::native().unwrap().triple();
    portable::fixtures::package(directory, target, portable::fixtures::files(target))
  }
  #[cfg(target_os = "linux")]
  {
    let path = directory.join("ctmux.AppImage");
    fs::write(&path, linux::fixture_bytes()).unwrap();
    path
  }
  #[cfg(target_os = "macos")]
  fixture_macos_archive(directory)
}

async fn verify_install_update_and_repair(archive: &Path, supports_named_leases: bool) {
  #[cfg(not(windows))]
  let home = Home::new();
  #[cfg(windows)]
  let home = {
    let path = std::env::temp_dir().join(format!("ctlup app 彩-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path).unwrap();
    Home(path)
  };
  let store = Installations::new(&home.0);
  if supports_named_leases {
    let work = activation::Workspace::begin(&home.0).unwrap();
    let metadata = DesktopMetadata::provided(DesktopTarget::native().unwrap());
    let mut package = prepare_supplied(&work, archive, metadata).unwrap();
    package.naming.installation_layout_version = Some(DESKTOP_OBJECT_PATH_LAYOUT_VERSION);
    activate(&work, None, &package, DesktopSource::Provided, false).unwrap();
  } else {
    install(&home.0, None, None, None, Some(archive), false)
      .await
      .unwrap();
  }
  let selected = store.desktop_selection().unwrap().unwrap();
  assert_eq!(selected.source, DesktopSource::Provided);
  assert_eq!(selected.content_id, package_digest(archive).unwrap().0);
  let object = store.desktop_object_path(&selected.content_id).unwrap();
  let name = object.file_name().unwrap().to_str().unwrap();
  if supports_named_leases {
    assert!(name.starts_with("provided-"));
  } else {
    assert_eq!(name, selected.content_id);
  }
  let status = app_status(&home.0, Some(selected.clone())).unwrap();
  assert!(!status.missing);
  assert_eq!(
    status.payload.as_deref(),
    Some(object.join(payload_name()).as_path())
  );
  assert!(status.launch_entry.exists());
  assert!(object.join("desktop.json").is_file());
  let cache = store
    .updater_root()
    .join("cache/downloads/desktop")
    .join(&selected.content_id);
  assert!(cache.join("package").is_file());

  #[cfg(unix)]
  assert_eq!(
    fs::canonicalize(&status.launch_entry).unwrap(),
    fs::canonicalize(object.join(payload_name())).unwrap()
  );
  #[cfg(windows)]
  {
    let target = dunce::canonicalize(object.join(payload_name()).join("ctmux-app.exe")).unwrap();
    let link = lnks::Shortcut::load(&status.launch_entry).unwrap();
    assert_eq!(link.target_path.as_deref(), Some(target.as_path()));
    assert_eq!(link.working_dir.as_deref(), target.parent());
    assert!(link.arguments.is_none());
    assert_eq!(link.window_state, lnks::WindowState::Normal);
  }
  fs::remove_dir_all(&object).unwrap();
  assert!(app_status(&home.0, Some(selected.clone())).unwrap().missing);
  update(&home.0, false, false).await.unwrap();
  assert_eq!(store.desktop_selection().unwrap(), Some(selected));
  assert!(object.join(payload_name()).exists());
  assert!(
    !app_status(&home.0, store.desktop_selection().unwrap())
      .unwrap()
      .missing
  );
  assert!(!store.root().join("current").exists());
  assert!(!store.root().join("cache").exists());

  fs::remove_dir_all(&object).unwrap();
  with_current_launch_entry(&home.0, || store.prune_all(true)).unwrap();
  assert!(store.desktop_selection().unwrap().is_none());
  assert!(fs::symlink_metadata(status.launch_entry).is_err());
  assert!(!cache.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn signing_defaults_to_the_selected_tagged_desktop_version() {
  let stable = DesktopSource::Channel {
    channel: "stable".into(),
  };
  let pinned = DesktopSource::Release {
    publication_id: "0.1.4-42-1".into(),
  };
  let mut metadata = DesktopMetadata {
    app_version: Some("0.1.4".into()),
    bundle_id: Some("0.1.4".into()),
    publication_id: Some("0.1.4-42-1".into()),
    signing: Signing::Unsigned,
    ..DesktopMetadata::provided(DesktopTarget::native().unwrap())
  };
  assert_eq!(metadata.stable_version(&stable), Some("0.1.4"));
  assert_eq!(metadata.stable_version(&pinned), Some("0.1.4"));
  for source in [
    DesktopSource::Channel {
      channel: "nightly".into(),
    },
    DesktopSource::Release {
      publication_id: "0.1.4-44-1".into(),
    },
    DesktopSource::Checkout,
    DesktopSource::PullRequest { number: 42 },
    DesktopSource::Provided,
  ] {
    assert_eq!(metadata.stable_version(&source), None);
  }
  metadata.bundle_id = Some("0.1.4-abcdef0".into());
  assert_eq!(metadata.stable_version(&pinned), None);
}

#[cfg(target_os = "macos")]
fn fixture_macos_archive(directory: &Path) -> PathBuf {
  let app = directory.join("ctmux.app");
  let contents = app.join("Contents");
  let executable = contents.join("MacOS/ctmux-app");
  fs::create_dir_all(executable.parent().unwrap()).unwrap();
  fs::copy("/usr/bin/true", executable).unwrap();
  fs::write(
    contents.join("Info.plist"),
    br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.tokn-ai.ctl.ctmux</string>
<key>CFBundleExecutable</key><string>ctmux-app</string>
<key>CFBundleShortVersionString</key><string>0.1.4</string>
</dict></plist>"#,
  )
  .unwrap();
  let archive = directory.join("fixture.tar.gz");
  let status = std::process::Command::new("/usr/bin/tar")
    .args(["-czf"])
    .arg(&archive)
    .arg("-C")
    .arg(directory)
    .arg("ctmux.app")
    .status()
    .unwrap();
  assert!(status.success());
  archive
}
