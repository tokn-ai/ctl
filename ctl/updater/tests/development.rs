#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctlup-dev-cli-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    fs::create_dir_all(root.join("scripts/dev")).unwrap();
    fs::write(root.join("Cargo.toml"), "fixture").unwrap();
    fs::write(
      root.join("scripts/dev/build-local-development.mts"),
      "fixture",
    )
    .unwrap();
    let builder = root.join("builder");
    fs::write(
      &builder,
      "#!/bin/sh\nsleep 0.1\nprintf 'fixture build failure\\n' >&2\nexit 17\n",
    )
    .unwrap();
    fs::set_permissions(builder, fs::Permissions::from_mode(0o700)).unwrap();
    Self(root)
  }

  fn command(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctlup"));
    command
      .args(["dev", "--workspace"])
      .arg(&self.0)
      .env("HOME", &self.0)
      .env("USERPROFILE", &self.0)
      .env("CTL_NODE_PROGRAM", self.0.join("builder"))
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    command
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn finish(mut child: Child) -> Output {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    if child.try_wait().unwrap().is_some() {
      return child.wait_with_output().unwrap();
    }
    if Instant::now() >= deadline {
      child.kill().unwrap();
      let output = child.wait_with_output().unwrap();
      panic!(
        "ctlup did not exit while its stdin pipe remained open: {}",
        String::from_utf8_lossy(&output.stderr)
      );
    }
    std::thread::sleep(Duration::from_millis(10));
  }
}

#[test]
fn failed_development_preparation_exits_while_the_launcher_holds_stdin_open() {
  let fixture = Fixture::new();
  let mut child = fixture
    .command()
    .args(["--json", "--lease"])
    .stdin(Stdio::piped())
    .spawn()
    .unwrap();
  let writer = child.stdin.take().unwrap();
  let output = finish(child);
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("fixture build failure"));
  assert_eq!(output.stdout, Vec::<u8>::new());
  assert!(!fixture.0.join(".tokn/ctl/current").exists());
  drop(writer);
}

#[test]
fn node_launcher_socket_stdin_is_cancellable_when_the_builder_fails() {
  let fixture = Fixture::new();
  let launcher = fixture.0.join("launcher.cjs");
  fs::write(&launcher, r#"
const { spawn } = require("node:child_process");
const child = spawn(process.env.FIXTURE_CTLUP, ["dev", "--workspace", process.env.HOME, "--json", "--lease"], {
  stdio: ["pipe", "pipe", "pipe"],
});
child.stdout.pipe(process.stdout);
child.stderr.pipe(process.stderr);
let timed_out = false;
const timer = setTimeout(() => { timed_out = true; child.kill("SIGKILL"); }, 3000);
child.once("error", (error) => { console.error(error); process.exitCode = 1; clearTimeout(timer); });
child.once("close", (code) => {
  clearTimeout(timer);
  child.stdin.end();
  process.exitCode = timed_out || code === 0 ? 1 : 0;
});
"#).unwrap();
  let output = finish(
    Command::new("node")
      .arg(&launcher)
      .env("FIXTURE_CTLUP", env!("CARGO_BIN_EXE_ctlup"))
      .env("HOME", &fixture.0)
      .env("USERPROFILE", &fixture.0)
      .env("CTL_NODE_PROGRAM", fixture.0.join("builder"))
      .stdin(Stdio::null())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap(),
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(String::from_utf8_lossy(&output.stderr).contains("fixture build failure"));
  assert_eq!(output.stdout, Vec::<u8>::new());
  assert!(!fixture.0.join(".tokn/ctl/current").exists());
}

#[test]
fn private_development_lease_rejects_non_pipe_input_before_building() {
  let fixture = Fixture::new();
  let output = finish(
    fixture
      .command()
      .args(["--json", "--lease"])
      .stdin(Stdio::null())
      .spawn()
      .unwrap(),
  );
  assert!(!output.status.success());
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(
    diagnostics.contains("development lease requires a readable stdin pipe"),
    "{diagnostics}"
  );
  assert!(!diagnostics.contains("fixture build failure"));
}

#[test]
fn development_preparation_rejects_remote_targets_before_building() {
  let fixture = Fixture::new();
  let output = finish(
    fixture
      .command()
      .args(["--destination", "fixture-host"])
      .stdin(Stdio::null())
      .spawn()
      .unwrap(),
  );
  assert!(!output.status.success());
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(diagnostics.contains("local"), "{diagnostics}");
  assert!(!diagnostics.contains("fixture build failure"));
}
