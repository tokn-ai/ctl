#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!(
      "cm-{}",
      &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }
  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(args)
      .env("HOME", &self.0)
      .env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"))
      .env("CTMUX_RUNTIME_DIR", &self.0)
      .env("CTL_TASKD_RUNTIME_DIR", &self.0)
      .env("CTLD_BIN", self.0.join("missing-ctld"))
      .env("CTMUXD_BIN", self.0.join("missing-ctmuxd"))
      .env("CTL_TASKD_BIN", self.0.join("missing-taskd"));
    command
  }
  fn assert_untouched(&self) {
    assert_eq!(
      std::fs::read_dir(&self.0).unwrap().count(),
      0,
      "maintenance unexpectedly created a service endpoint or installation"
    );
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    std::fs::remove_dir_all(&self.0).unwrap();
  }
}

#[test]
fn status_preserves_absent_owners_and_reports_missing_replacements_individually() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let output = fixture
    .command(&["components", "status", "--json"])
    .output()
    .unwrap();
  assert!(!output.status.success());
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  let rows = value["components"].as_array().unwrap();
  assert_eq!(rows.len(), 3);
  for row in rows {
    assert_eq!(row["state"], "not_running");
    assert!(row["running"].is_null());
    assert!(row["available"].is_null());
    assert!(row["restart_needed"].is_null());
    assert_eq!(row["errors"].as_array().unwrap().len(), 1);
  }
  fixture.assert_untouched();
}

#[test]
fn unattended_and_unsupported_restarts_fail_before_io() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  for (args, message) in [
    (vec!["components", "restart", "ctld"], "explicit --yes"),
    (
      vec![
        "-H",
        "does-not-exist",
        "components",
        "restart",
        "ctld",
        "--yes",
      ],
      "only ctmuxd",
    ),
    (
      vec![
        "-H",
        "does-not-exist",
        "components",
        "restart",
        "ctl-taskd",
        "--yes",
      ],
      "only ctmuxd",
    ),
    (
      vec!["--method", "route", "components", "status"],
      "--method requires --host",
    ),
  ] {
    let output = fixture.command(&args).output().unwrap();
    assert!(!output.status.success());
    assert!(
      String::from_utf8_lossy(&output.stderr).contains(message),
      "{output:?}"
    );
    fixture.assert_untouched();
  }
}

#[test]
fn preflight_failure_never_starts_an_absent_owner() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  for daemon in ["ctld", "ctmuxd", "ctl-taskd"] {
    let output = fixture
      .command(&["components", "restart", daemon, "--dry-run", "--json"])
      .output()
      .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    fixture.assert_untouched();
  }
}

#[tokio::test]
async fn status_reads_a_running_owner_without_sending_restart() {
  use ctl_ipc::lifecycle::{DaemonBinaryInfo, DaemonInfo, Request, Response};
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let socket = fixture.0.join("ctld.sock");
  let listener = tokio::net::UnixListener::bind(&socket).unwrap();
  let info = DaemonInfo {
    instance_id: uuid::Uuid::new_v4().to_string(),
    binary: DaemonBinaryInfo::current(),
    active_vpn_count: 2,
  };
  let expected_build = info.binary.build.clone();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, Request>(&mut stream)
        .await
        .unwrap(),
      Some(Request::CtldInspect { .. })
    ));
    ctl_ipc::write_frame(
      &mut stream,
      &Response::CtldInfo {
        protocol_version: ctl_ipc::lifecycle::PROTOCOL_VERSION,
        info,
      },
    )
    .await
    .unwrap();
    assert!(
      ctl_ipc::read_frame::<_, Request>(&mut stream)
        .await
        .unwrap()
        .is_none()
    );
    assert!(
      tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
        .await
        .is_err()
    );
  });
  let mut command =
    tokio::process::Command::from(fixture.command(&["components", "status", "--json"]));
  command.kill_on_drop(true);
  let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
    .await
    .unwrap()
    .unwrap();
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  let broker = value["components"]
    .as_array()
    .unwrap()
    .iter()
    .find(|row| row["component"] == "ctld")
    .unwrap();
  assert_eq!(broker["state"], "running");
  assert_eq!(
    broker["running"]["build"],
    serde_json::to_value(expected_build).unwrap()
  );
  assert_eq!(broker["running_compatible"], true);
  assert!(broker["restart_needed"].is_null());
  tokio::time::timeout(std::time::Duration::from_secs(5), server)
    .await
    .unwrap()
    .unwrap();
  std::fs::remove_file(socket).unwrap();
  fixture.assert_untouched();
}
