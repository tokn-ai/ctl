use super::*;
use std::os::unix::fs::symlink;

struct Scratch(PathBuf);

impl Scratch {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-app-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn fixture(&self) -> PathBuf {
    let app = self.0.join(APP_NAME);
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::write(
      app.join("Contents/Info.plist"),
      r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>dev.tokn-ai.ctl.ctmux</string>
  <key>CFBundleExecutable</key><string>ctmux-app</string>
  <key>CFBundleShortVersionString</key><string>0.1.3</string>
</dict></plist>"#,
    )
    .unwrap();
    fs::copy("/bin/sleep", app.join(EXECUTABLE_PATH)).unwrap();
    app
  }

  fn archive(&self) -> PathBuf {
    let archive = self.0.join("desktop.tar.gz");
    require_success(
      Command::new("/usr/bin/tar")
        .arg("-czf")
        .arg(&archive)
        .args(["--mac-metadata", "-C"])
        .arg(&self.0)
        .arg(APP_NAME),
      "creating fixture archive",
    )
    .unwrap();
    archive
  }

  fn staging(&self) -> PathBuf {
    let staging = self.0.join("staging");
    fs::create_dir(&staging).unwrap();
    staging
  }
}

impl Drop for Scratch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn unpacks_bundle_with_executable_mode_symlink_and_macos_metadata() {
  let scratch = Scratch::new();
  let source = scratch.fixture();
  symlink("MacOS/ctmux-app", source.join("Contents/Link")).unwrap();
  require_success(
    Command::new("/usr/bin/xattr")
      .args(["-w", "dev.ctmux.fixture", "metadata"])
      .arg(&source),
    "setting fixture metadata",
  )
  .unwrap();

  let app = unpack(&scratch.archive(), &scratch.staging()).unwrap();
  assert_eq!(installed_version(&app).unwrap(), "0.1.3");
  assert_eq!(
    fs::read_link(app.join("Contents/Link")).unwrap(),
    Path::new("MacOS/ctmux-app")
  );
  assert!(
    fs::metadata(app.join(EXECUTABLE_PATH))
      .unwrap()
      .permissions()
      .mode()
      & 0o111
      != 0
  );
  let metadata = require_success(
    Command::new("/usr/bin/xattr")
      .args(["-p", "dev.ctmux.fixture"])
      .arg(&app),
    "reading extracted metadata",
  )
  .unwrap();
  assert_eq!(metadata.stdout, b"metadata\n");
}

#[test]
fn rejects_external_symlink_before_extracting_anything() {
  let scratch = Scratch::new();
  let source = scratch.fixture();
  symlink("../../unrelated", source.join("Contents/Link")).unwrap();
  let staging = scratch.staging();

  let error = unpack(&scratch.archive(), &staging).unwrap_err();
  assert!(error.to_string().contains("external symlink"));
  assert_eq!(fs::read_dir(staging).unwrap().count(), 0);
}

#[test]
fn rejects_paths_that_resolve_to_the_same_file_on_a_case_insensitive_volume() {
  let scratch = Scratch::new();
  scratch.fixture();
  fs::write(scratch.0.join("first"), b"first").unwrap();
  fs::write(scratch.0.join("second"), b"second").unwrap();
  let archive = scratch.0.join("aliases.tar.gz");
  require_success(
    Command::new("/usr/bin/tar")
      .arg("-czf")
      .arg(&archive)
      .arg("-C")
      .arg(&scratch.0)
      .args([
        "-s",
        ",^first$,ctmux.app/Contents/Case,",
        "-s",
        ",^second$,ctmux.app/Contents/case,",
        APP_NAME,
        "first",
        "second",
      ]),
    "creating fixture archive",
  )
  .unwrap();

  let extracted = unpack(&archive, &scratch.staging());
  if scratch.0.join("FIRST").exists() {
    assert!(extracted.unwrap_err().to_string().contains("alias"));
  } else {
    assert!(extracted.is_ok());
  }
}

#[test]
fn rejects_unsigned_app_at_the_expected_version() {
  let scratch = Scratch::new();
  scratch.fixture();
  let error = stage(
    &scratch.archive(),
    &scratch.staging(),
    "0.1.3",
    DesktopSigning::Signed,
  )
  .unwrap_err();
  assert!(
    error
      .to_string()
      .contains("Developer ID signature verification")
  );
}

#[test]
fn stages_an_app_from_an_unsigned_release() {
  let scratch = Scratch::new();
  scratch.fixture();
  let app = stage(
    &scratch.archive(),
    &scratch.staging(),
    "0.1.3",
    DesktopSigning::Unsigned,
  )
  .unwrap();
  assert_eq!(installed_version(&app).unwrap(), "0.1.3");
  assert!(verify_existing(&app, "0.1.3", DesktopSigning::Unsigned).is_ok());
  assert!(verify_existing(&app, "0.1.3", DesktopSigning::Signed).is_err());
}

#[test]
fn compares_app_contents_and_inventory_at_the_same_product_version() {
  let scratch = Scratch::new();
  let installed = scratch.fixture();
  let resource = "Contents/resource";
  fs::write(installed.join(resource), b"original").unwrap();
  let staged = unpack(&scratch.archive(), &scratch.staging()).unwrap();
  assert!(matches(&installed, &staged).unwrap());

  fs::write(installed.join(resource), b"modified").unwrap();
  assert_eq!(installed_version(&installed).unwrap(), "0.1.3");
  assert!(!matches(&installed, &staged).unwrap());
  fs::copy(staged.join(resource), installed.join(resource)).unwrap();
  assert!(matches(&installed, &staged).unwrap());

  let extra = installed.join("Contents/extra");
  fs::write(&extra, b"unexpected").unwrap();
  assert!(!matches(&installed, &staged).unwrap());
  fs::remove_file(extra).unwrap();
  fs::remove_file(installed.join(resource)).unwrap();
  assert!(!matches(&installed, &staged).unwrap());
  assert!(!matches(&scratch.0.join("missing.app"), &staged).unwrap());
}

#[test]
fn compares_executable_permissions_and_symlink_destinations() {
  let scratch = Scratch::new();
  let installed = scratch.fixture();
  let link = installed.join("Contents/Link");
  symlink("MacOS/ctmux-app", &link).unwrap();
  let staged = unpack(&scratch.archive(), &scratch.staging()).unwrap();
  assert!(matches(&installed, &staged).unwrap());

  let executable = installed.join(EXECUTABLE_PATH);
  let permissions = fs::metadata(&executable).unwrap().permissions();
  fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
  assert!(!matches(&installed, &staged).unwrap());
  fs::set_permissions(&executable, permissions).unwrap();
  assert!(matches(&installed, &staged).unwrap());

  fs::remove_file(&link).unwrap();
  symlink("Info.plist", &link).unwrap();
  assert!(!matches(&installed, &staged).unwrap());
}

#[test]
fn never_traverses_an_installed_directory_replaced_with_a_symlink() {
  let scratch = Scratch::new();
  let installed = scratch.fixture();
  let staged = unpack(&scratch.archive(), &scratch.staging()).unwrap();
  fs::remove_dir_all(installed.join("Contents/MacOS")).unwrap();
  symlink("MacOS", installed.join("Contents/MacOS")).unwrap();

  assert!(!matches(&installed, &staged).unwrap());
}

#[test]
fn identifies_the_running_bundle_by_its_executable_path() {
  let scratch = Scratch::new();
  let app = scratch.fixture();
  let other_scratch = Scratch::new();
  let other_app = other_scratch.fixture();
  assert!(!is_running(&app).unwrap());

  let mut child = Command::new(app.join(EXECUTABLE_PATH))
    .arg("30")
    .spawn()
    .unwrap();
  let running = is_running(&app);
  let other_running = is_running(&other_app);
  let _ = child.kill();
  child.wait().unwrap();

  assert!(running.unwrap());
  assert!(!other_running.unwrap());
  assert!(!is_running(&app).unwrap());
}
