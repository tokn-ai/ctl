use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

const REMOTE_ID: &str = "11111111-1111-4111-8111-111111111111";

struct Fixture {
  root: PathBuf,
  home: PathBuf,
  bin: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctl-pin-read-{}", uuid::Uuid::new_v4()));
    let home = root.join("home with spaces");
    let bin = root.join("bin");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir(&bin).unwrap();
    // The remote environment deliberately has no ctl-agent or daemon binaries.
    for program in ["sh", "dd"] {
      let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join(program))
        .find(|path| path.is_file())
        .unwrap();
      symlink(executable, bin.join(program)).unwrap();
    }
    Self { root, home, bin }
  }

  fn path(&self) -> PathBuf {
    self.home.join(".tokn/ctl/remote-id")
  }

  fn write(&self, bytes: &[u8]) {
    fs::create_dir_all(self.path().parent().unwrap()).unwrap();
    fs::write(self.path(), bytes).unwrap();
  }

  fn command(&self, prelude: &str) -> Command {
    let mut command = Command::new(self.bin.join("sh"));
    command
      .args(["-c", &format!("{prelude}{}", remote_command())])
      .env("HOME", &self.home)
      .env("PATH", &self.bin);
    command
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn reads_the_existing_pin_without_an_agent_and_preserves_storage() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fixture.write(REMOTE_ID.as_bytes());
  let id = inspect(fixture.command("printf 'Welcome\\nloading profile'; "))
    .await
    .unwrap();
  assert_eq!(id, REMOTE_ID);
  assert_eq!(fs::read(fixture.path()).unwrap(), REMOTE_ID.as_bytes());
  for directory in [
    fixture.home.clone(),
    fixture.home.join(".tokn"),
    fixture.home.join(".tokn/ctl"),
  ] {
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
  }
  assert_eq!(fs::read_dir(&fixture.bin).unwrap().count(), 2);
}

#[tokio::test]
async fn missing_identity_is_not_created() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let error = inspect(fixture.command("")).await.unwrap_err();
  assert!(
    matches!(&error, CoreError::SshCommandFailed { diagnostic, .. }
    if diagnostic.contains("saved ctl account identity is missing or is not a regular file"))
  );
  assert_eq!(fs::read_dir(&fixture.home).unwrap().count(), 0);
}

#[tokio::test]
async fn malformed_or_oversized_pins_are_rejected_without_rewriting_them() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let mut oversized = REMOTE_ID.as_bytes().to_vec();
  oversized.extend(vec![b'x'; 64 * 1024]);
  for bytes in [
    Vec::new(),
    b"invalid".to_vec(),
    b"00000000-0000-0000-0000-000000000000".to_vec(),
    b"AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA".to_vec(),
    format!("{REMOTE_ID}\n").into_bytes(),
    format!(" {REMOTE_ID}").into_bytes(),
    vec![0xff; 36],
    oversized,
  ] {
    fixture.write(&bytes);
    assert!(matches!(
      inspect(fixture.command("")).await,
      Err(CoreError::InvalidSshCommandOutput)
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
  }
  let output = crate::run_marked_fixed_command(fixture.command(""), &[], MARKER)
    .await
    .unwrap();
  assert_eq!(output.len(), 37);
}

#[tokio::test]
async fn non_regular_identity_files_fail_without_waiting_for_a_writer() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.path()).unwrap();
  assert!(matches!(
    inspect(fixture.command("")).await,
    Err(CoreError::SshCommandFailed { .. })
  ));
  fs::remove_dir(fixture.path()).unwrap();
  let status = Command::new("mkfifo")
    .arg(fixture.path())
    .status()
    .await
    .unwrap();
  assert!(status.success());
  assert!(matches!(
    tokio::time::timeout(Duration::from_secs(2), inspect(fixture.command("")))
      .await
      .expect("a FIFO must fail before dd can block"),
    Err(CoreError::SshCommandFailed { .. })
  ));
  assert!(fs::symlink_metadata(fixture.path()).is_ok());
}

#[tokio::test]
async fn valid_output_does_not_override_a_failed_remote_command() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut command = Command::new("sh");
  command.args([
    "-c",
    &format!("printf 'ctl-command-v1\\n{REMOTE_ID}'; exit 17"),
  ]);
  assert!(matches!(
    inspect(command).await,
    Err(CoreError::SshCommandFailed { .. })
  ));
}

#[test]
fn identity_inspection_reuses_only_the_selected_master_without_service_commands() {
  let command = command(
    "configured-host",
    &SshConnectionOptions::default(),
    Path::new("/unused/owned-master"),
  )
  .unwrap();
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|arg| arg.to_string_lossy().into_owned())
    .collect();
  for pair in [
    ["-S", "/unused/owned-master"],
    ["-o", "ControlMaster=no"],
    ["-o", "BatchMode=yes"],
    ["-o", "ProxyCommand=false"],
  ] {
    assert!(args.windows(2).any(|values| values == pair));
  }
  assert_eq!(args.last().unwrap(), &remote_command());
  assert!(remote_command().starts_with("exec sh -c "));
  for executable in ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"] {
    assert!(!remote_command().contains(executable));
  }
}
