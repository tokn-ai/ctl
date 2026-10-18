use std::{path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("caller-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    std::fs::create_dir(root.join("home")).unwrap();
    std::fs::write(
      root.join("invalid-hosts.json"),
      "invalid catalog; temporary targets must not read this",
    )
    .unwrap();
    std::fs::write(root.join("key"), "synthetic-key-file").unwrap();
    std::fs::write(root.join("ssh.rs"), include_str!("fixtures/caller_ssh.rs")).unwrap();
    let result = Command::new("rustc")
      .args(["--edition=2024", "--crate-name", "caller_ssh_fixture"])
      .arg(root.join("ssh.rs"))
      .arg("-o")
      .arg(root.join(format!("ssh{}", std::env::consts::EXE_SUFFIX)))
      .output()
      .unwrap();
    assert!(
      result.status.success(),
      "{}",
      String::from_utf8_lossy(&result.stderr)
    );
    Self(root)
  }
  fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut paths = vec![self.0.clone()];
    paths.extend(std::env::split_paths(
      &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = Command::new(program);
    command
      .env("PATH", std::env::join_paths(paths).unwrap())
      .env("HOME", self.0.join("home"))
      .env("USERPROFILE", self.0.join("home"))
      .env("CTL_HOSTS_PATH", self.0.join("invalid-hosts.json"))
      .env("CTLD_SOCKET_PATH", self.0.join("never-start.sock"));
    command
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[cfg(unix)]
#[test]
fn socket_handoff_rejects_public_directories_and_preserves_existing_entries() {
  use std::os::unix::fs::PermissionsExt as _;
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let directory = std::path::PathBuf::from(format!(
    "/tmp/ctl-input-{}",
    &uuid::Uuid::new_v4().simple().to_string()[..12]
  ));
  std::fs::create_dir(&directory).unwrap();
  let path = directory.join("password.sock");
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
  let output = fixture
    .command(env!("CARGO_BIN_EXE_ctl"))
    .args(["--destination", "host", "--password-socket"])
    .arg(&path)
    .args(["exec", "--", "true"])
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("0700"));
  assert!(!path.exists());
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  std::fs::write(&path, "existing application data").unwrap();
  let output = fixture
    .command(env!("CARGO_BIN_EXE_ctl"))
    .args(["--destination", "host", "--password-socket"])
    .arg(&path)
    .args(["exec", "--", "true"])
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert_eq!(
    std::fs::read_to_string(&path).unwrap(),
    "existing application data"
  );
  std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn caller_key_mode_isolated_from_managed_authentication_and_config_identities() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  for paths in ["normal", "default-space", "custom-space"] {
    let output = fixture
      .command(env!("CARGO_BIN_EXE_ctl"))
      .env("CTL_TEST_CONFIG_PATHS", paths)
      .args(["--destination", "work", "--identity-file"])
      .arg(fixture.0.join("key"))
      .args(["exec", "--", "uname", "-a"])
      .output()
      .unwrap();
    if paths == "custom-space" {
      assert_eq!(output.status.code(), Some(1), "{output:?}");
      assert!(String::from_utf8_lossy(&output.stderr).contains("unambiguously"));
    } else {
      assert_eq!(output.status.code(), Some(17), "{output:?}");
      assert!(String::from_utf8_lossy(&output.stderr).contains("separate-command-stderr"));
    }
  }
  assert!(!fixture.0.join("never-start.sock").exists());
  assert_eq!(
    std::fs::read_dir(fixture.0.join("home")).unwrap().count(),
    0
  );
}

#[test]
#[ignore = "requires Bun; explicitly exercised on Windows, macOS and Linux in CI"]
fn bun_password_handoff_preserves_stdin_and_does_not_disclose_or_persist_secrets() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let script = fixture.0.join("handoff.mts");
  std::fs::write(&script, include_str!("fixtures/caller_handoff.mts")).unwrap();
  let output = fixture
    .command("bun")
    .arg(script)
    .arg(env!("CARGO_BIN_EXE_ctl"))
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert!(!fixture.0.join("never-start.sock").exists());
  assert_eq!(
    std::fs::read_dir(fixture.0.join("home")).unwrap().count(),
    0
  );
}

#[cfg(unix)]
#[test]
#[ignore = "requires Node; explicitly exercised in Unix CI"]
fn inherited_descriptor_handoff_preserves_stdin_and_rejects_missing_eof() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let script = fixture.0.join("handoff.mts");
  std::fs::write(&script, include_str!("fixtures/caller_handoff.mts")).unwrap();
  let output = fixture
    .command("node")
    .arg("--experimental-strip-types")
    .arg(script)
    .arg(env!("CARGO_BIN_EXE_ctl"))
    .arg("--fd")
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert!(!fixture.0.join("never-start.sock").exists());
  assert_eq!(
    std::fs::read_dir(fixture.0.join("home")).unwrap().count(),
    0
  );
}
