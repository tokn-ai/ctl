use super::*;
use std::os::unix::fs::symlink;

struct Scratch(PathBuf);

impl Scratch {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-app-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }
}

impl Drop for Scratch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn install_never_claims_a_destination_created_after_staging() {
  let scratch = Scratch::new();
  let destination = scratch.0.join("ctmux.app");
  let mut work = Workspace::begin(&destination).unwrap();
  let staged = work.staging().join("ctmux.app");
  fs::create_dir(&staged).unwrap();
  fs::create_dir(&destination).unwrap();
  fs::write(destination.join("owned"), b"other install").unwrap();

  let error = work.activate(&staged, None).unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
  assert_eq!(
    fs::read(destination.join("owned")).unwrap(),
    b"other install"
  );
  assert!(staged.is_dir());
}

#[test]
fn exchanges_nonempty_bundles_and_retains_the_immediate_previous_one() {
  let scratch = Scratch::new();
  let destination = scratch.0.join("ctmux.app");
  for version in 1..=3 {
    let mut work = Workspace::begin(&destination).unwrap();
    let existing = existing(&destination).unwrap();
    let staged = work.staging().join("ctmux.app");
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("version"), [version]).unwrap();

    let previous = work.activate(&staged, existing.as_ref()).unwrap();
    drop(work);
    assert!(!staged.exists());
    assert_eq!(fs::read(destination.join("version")).unwrap(), [version]);
    if version == 1 {
      assert!(previous.is_none());
    } else {
      let previous = previous.unwrap();
      assert_eq!(previous.file_name().unwrap(), "previous");
      assert_eq!(fs::read(previous.join("version")).unwrap(), [version - 1]);
    }
  }
}

#[test]
fn exchanges_regular_appimage_files() {
  let scratch = Scratch::new();
  let destination = scratch.0.join("ctmux.AppImage");
  fs::write(&destination, b"old").unwrap();
  let mut work = Workspace::begin(&destination).unwrap();
  let existing = existing(&destination).unwrap().unwrap();
  let staged = work.staging().join("ctmux.AppImage");
  fs::write(&staged, b"new").unwrap();

  let previous = work.activate(&staged, Some(&existing)).unwrap().unwrap();
  drop(work);
  assert_eq!(fs::read(&destination).unwrap(), b"new");
  assert_eq!(fs::read(&previous).unwrap(), b"old");
}

#[test]
fn refuses_to_replace_a_destination_changed_since_verification() {
  let scratch = Scratch::new();
  let destination = scratch.0.join("ctmux.AppImage");
  fs::write(&destination, b"verified").unwrap();
  let mut work = Workspace::begin(&destination).unwrap();
  let existing = existing(&destination).unwrap().unwrap();
  fs::rename(&destination, scratch.0.join("earlier")).unwrap();
  symlink("earlier", &destination).unwrap();
  let staged = work.staging().join("ctmux.AppImage");
  fs::write(&staged, b"update").unwrap();

  assert!(work.activate(&staged, Some(&existing)).is_err());
  assert_eq!(fs::read_link(destination).unwrap(), Path::new("earlier"));
}

#[test]
fn only_one_workspace_can_stage_an_update_for_a_destination() {
  let scratch = Scratch::new();
  let destination = scratch.0.join("ctmux.app");
  let work = Workspace::begin(&destination).unwrap();

  let error = Workspace::begin(&destination).err().unwrap();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  drop(work);
  assert!(Workspace::begin(&destination).is_ok());
}
