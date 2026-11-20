#![cfg(unix)]

use std::{
  fs,
  os::unix::fs::PermissionsExt,
  path::PathBuf,
  process::Command,
  time::{SystemTime, UNIX_EPOCH},
};

struct Fixture(PathBuf);

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn windows_query_reaches_the_selected_agent_with_the_window_contract() {
  let stamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let fixture =
    Fixture(std::env::temp_dir().join(format!("ctream-cli-window-{}-{stamp}", std::process::id())));
  fs::create_dir(&fixture.0).unwrap();
  let agent = fixture.0.join("test agent");
  fs::write(
    &agent,
    r##"#!/bin/sh
set -eu
test "$#" -eq 3
test "$1" = 1.0.6
test "$2" = windows
test "$3" = windows-2
printf '%s\n' '[{"id":"win-test","title":"Test window","app_id":"fixture"}]'
"##,
  )
  .unwrap();
  fs::set_permissions(&agent, fs::Permissions::from_mode(0o700)).unwrap();
  let output = Command::new(env!("CARGO_BIN_EXE_ctream"))
    .args([
      "--agent",
      agent.to_str().unwrap(),
      "windows",
      "windows-2",
      "--json",
    ])
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let windows: Vec<ctream_proto::Window> = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(windows.len(), 1);
  assert_eq!(windows[0].id, "win-test");
}
