use super::*;
use std::fs::OpenOptions;

struct Scratch(PathBuf);
impl Scratch {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctlup-activation-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    Self(root)
  }
  fn destination(&self) -> PathBuf {
    self.0.join("ctmux")
  }
}
impl Drop for Scratch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn payload(workspace: &Workspace, bytes: &[u8]) -> PathBuf {
  let root = workspace.staging().join("ctmux");
  fs::create_dir(&root).unwrap();
  fs::write(root.join("fixture"), bytes).unwrap();
  fs::create_dir(root.join("nested")).unwrap();
  fs::write(root.join("nested/resource"), bytes).unwrap();
  root
}

#[test]
fn serializes_installers_and_retains_the_immediate_previous_app() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  for version in 1..=3 {
    let mut work = Workspace::begin(&destination).unwrap();
    assert!(Workspace::begin(&destination).is_err());
    let expected = existing(&destination).unwrap();
    let staged = payload(&work, &[version]);
    let previous = work.activate(&staged, expected.as_ref()).unwrap();
    assert_eq!(fs::read(destination.join("fixture")).unwrap(), [version]);
    assert_eq!(
      fs::read(destination.join("nested/resource")).unwrap(),
      [version]
    );
    if version > 1 {
      assert_eq!(
        fs::read(previous.unwrap().join("fixture")).unwrap(),
        [version - 1]
      );
    }
  }
}

#[test]
fn refuses_locked_payload_without_disturbing_the_installed_app() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  fs::create_dir(&destination).unwrap();
  fs::write(destination.join("fixture"), b"original").unwrap();
  let busy = OpenOptions::new()
    .read(true)
    .share_mode(1)
    .open(destination.join("fixture"))
    .unwrap();
  let mut work = Workspace::begin(&destination).unwrap();
  let staged = payload(&work, b"replacement");
  let expected = existing(&destination).unwrap();
  let error = work.activate(&staged, expected.as_ref()).unwrap_err();
  assert!(error.to_string().contains("Close the ctmux desktop app"));
  assert_eq!(fs::read(destination.join("fixture")).unwrap(), b"original");
  drop(busy);
}

#[test]
fn rolls_back_if_publishing_the_replacement_fails() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  fs::create_dir(&destination).unwrap();
  fs::write(destination.join("fixture"), b"original").unwrap();
  let mut work = Workspace::begin(&destination).unwrap();
  let staged = payload(&work, b"replacement");
  // Disallow renaming the staged tree, after the old tree has been retained.
  let busy = OpenOptions::new()
    .read(true)
    .share_mode(1)
    .open(staged.join("fixture"))
    .unwrap();
  let expected = existing(&destination).unwrap();
  let error = work.activate(&staged, expected.as_ref()).unwrap_err();
  assert!(
    error
      .to_string()
      .contains("could not publish the staged app"),
    "{error}"
  );
  assert_eq!(fs::read(destination.join("fixture")).unwrap(), b"original");
  assert!(!work.storage.join("pending-previous").exists());
  drop(busy);
}

#[test]
fn recovers_an_interrupted_update_before_inspecting_the_destination() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  let work = Workspace::begin(&destination).unwrap();
  let pending = work.storage.join("pending-previous");
  fs::create_dir(&pending).unwrap();
  fs::write(pending.join("fixture"), b"original").unwrap();
  drop(work);
  let work = Workspace::begin(&destination).unwrap();
  assert_eq!(fs::read(destination.join("fixture")).unwrap(), b"original");
  assert!(!work.storage.join("pending-previous").exists());
}

#[test]
fn completes_previous_retention_after_an_interrupted_successful_update() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  let work = Workspace::begin(&destination).unwrap();
  let pending = work.storage.join("pending-previous");
  fs::create_dir(&pending).unwrap();
  fs::write(pending.join("fixture"), b"original").unwrap();
  fs::create_dir(&destination).unwrap();
  fs::write(destination.join("fixture"), b"replacement").unwrap();
  drop(work);
  let work = Workspace::begin(&destination).unwrap();
  assert_eq!(
    fs::read(destination.join("fixture")).unwrap(),
    b"replacement"
  );
  assert_eq!(
    fs::read(work.storage.join("previous/fixture")).unwrap(),
    b"original"
  );
}

#[test]
fn fresh_install_preserves_a_concurrently_created_destination() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  let mut work = Workspace::begin(&destination).unwrap();
  let staged = payload(&work, b"replacement");
  fs::create_dir(&destination).unwrap();
  fs::write(destination.join("fixture"), b"concurrent").unwrap();
  assert_eq!(
    work.activate(&staged, None).unwrap_err().kind(),
    io::ErrorKind::AlreadyExists
  );
  assert_eq!(
    fs::read(destination.join("fixture")).unwrap(),
    b"concurrent"
  );
}

#[test]
fn retains_all_files_when_an_older_previous_build_is_busy() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  for version in 1..=2 {
    let mut work = Workspace::begin(&destination).unwrap();
    let expected = existing(&destination).unwrap();
    let staged = payload(&work, &[version]);
    work.activate(&staged, expected.as_ref()).unwrap();
  }
  let storage = scratch.0.join(".ctlup-ctmux");
  let busy = OpenOptions::new()
    .read(true)
    .share_mode(1)
    .open(storage.join("previous/fixture"))
    .unwrap();
  fs::write(storage.join("previous/resource"), b"keep this resource").unwrap();
  {
    let mut work = Workspace::begin(&destination).unwrap();
    let expected = existing(&destination).unwrap();
    let staged = payload(&work, &[3]);
    let retained = work.activate(&staged, expected.as_ref()).unwrap().unwrap();
    assert_eq!(retained, storage.join("pending-previous"));
    assert_eq!(fs::read(retained.join("fixture")).unwrap(), [2]);
    assert_eq!(fs::read(destination.join("fixture")).unwrap(), [3]);
    assert_eq!(
      fs::read(storage.join("previous/resource")).unwrap(),
      b"keep this resource"
    );
  }
  assert!(Workspace::begin(&destination).is_err());
  drop(busy);
  let work = Workspace::begin(&destination).unwrap();
  assert_eq!(
    fs::read(work.storage.join("previous/fixture")).unwrap(),
    [2]
  );
  assert!(!work.storage.join("pending-previous").exists());
}

#[test]
fn refuses_a_running_image_without_moving_the_installed_app() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  fs::create_dir(&destination).unwrap();
  let executable = destination.join("running-fixture.exe");
  fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
  let mut child = std::process::Command::new(&executable)
    .args([
      "--ignored",
      "--exact",
      "updater::app::activation::tests::hold_windows_image_for_activation_test",
    ])
    .env("CTLUP_TEST_RUNNING_IMAGE_CHILD", "1")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::null())
    .spawn()
    .unwrap();
  let mut work = Workspace::begin(&destination).unwrap();
  let staged = payload(&work, b"replacement");
  let expected = existing(&destination).unwrap();
  let result = work.activate(&staged, expected.as_ref());
  // End the disposable fixture process before assertions/cleanup.
  let running = child.try_wait().unwrap().is_none();
  drop(child.stdin.take());
  let status = child.wait().unwrap();
  assert!(running && status.success());
  assert!(
    result
      .unwrap_err()
      .to_string()
      .contains("Close the ctmux desktop app")
  );
  assert!(executable.is_file());
  assert!(!work.storage.join("pending-previous").exists());
}

#[test]
#[ignore = "child fixture held open by its parent activation test"]
fn hold_windows_image_for_activation_test() {
  use std::io::Read as _;
  if std::env::var_os("CTLUP_TEST_RUNNING_IMAGE_CHILD").is_none() {
    return;
  }
  let _ = std::io::stdin().read(&mut [0_u8]);
}

#[test]
fn a_post_move_busy_file_preserves_rollback_until_recovery() {
  let scratch = Scratch::new();
  let destination = scratch.destination();
  let work = Workspace::begin(&destination).unwrap();
  let staged = payload(&work, b"replacement");
  let pending = work.storage.join("pending-previous");
  fs::create_dir(&pending).unwrap();
  fs::write(pending.join("fixture"), b"original").unwrap();
  // Simulate a process acquiring a file after the installed tree was moved.
  let busy = OpenOptions::new()
    .read(true)
    .share_mode(1)
    .open(pending.join("fixture"))
    .unwrap();
  let error = work.publish_replacement(&staged, &pending).unwrap_err();
  assert!(
    error.to_string().contains("Close the ctmux desktop app"),
    "{error}"
  );
  assert!(!destination.exists());
  assert_eq!(fs::read(pending.join("fixture")).unwrap(), b"original");
  assert_eq!(fs::read(staged.join("fixture")).unwrap(), b"replacement");
  drop(busy);
  drop(work);
  let recovered = Workspace::begin(&destination).unwrap();
  assert_eq!(fs::read(destination.join("fixture")).unwrap(), b"original");
  assert!(!recovered.storage.join("pending-previous").exists());
}
