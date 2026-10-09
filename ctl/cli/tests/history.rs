use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-history-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    Self(path)
  }
  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(args)
      .env("HOME", &self.0)
      .env("USERPROFILE", &self.0)
      .env("CTLD_BIN", self.0.join("must-not-start"))
      .env("CTLD_SOCKET_PATH", self.0.join("must-not-bind.sock"));
    command
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[cfg(unix)]
#[test]
fn reading_missing_history_and_path_never_creates_files_or_starts_ctld() {
  let fixture = Fixture::new();
  for stream in ["logs", "audit"] {
    let output = fixture.command(&[stream, "--json"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
      serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
      serde_json::json!({"records": [], "complete": true, "warning": null})
    );
    assert_eq!(output.stderr, Vec::<u8>::new());
    let output = fixture.command(&[stream, "--path"]).output().unwrap();
    assert!(output.status.success());
    assert!(
      String::from_utf8(output.stdout)
        .unwrap()
        .contains(".tokn/ctl/history")
    );
  }
  assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
}

#[test]
fn history_rejects_remote_target_and_invalid_limits_before_connecting() {
  let fixture = Fixture::new();
  for args in [
    vec!["-H", "work", "audit"],
    vec!["logs", "--limit", "0"],
    vec!["audit", "--limit", "10001"],
    vec!["logs", "--json", "--path"],
  ] {
    let output = fixture.command(&args).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(output.stdout, Vec::<u8>::new());
  }
  assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn populated_history_filters_failures_and_reports_omitted_records_in_json_and_table() {
  use std::fmt::Write as _;
  use std::os::unix::fs::PermissionsExt as _;
  let fixture = Fixture::new();
  let directory = fixture.0.join(".tokn/ctl/history");
  fs::create_dir_all(&directory).unwrap();
  fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
  let mut content = String::new();
  for outcome in ["failed", "succeeded", "interrupted"] {
    let record = serde_json::json!({
      "schema_version": 1, "event_id": uuid::Uuid::new_v4(),
      "operation_id": uuid::Uuid::new_v4(), "timestamp_ms": 0, "process_id": 42,
      "event": "connection", "outcome": outcome, "subject_id": "a".repeat(64),
      "elapsed_ms": 123, "error_code": null, "os_error": null
    });
    writeln!(content, "{record}").unwrap();
  }
  content.push_str("{bad record}\n");
  for (name, bytes) in [("history.lock", ""), ("audit.jsonl", content.as_str())] {
    let path = directory.join(name);
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
  }
  let output = fixture
    .command(&["audit", "--json", "--failed", "--limit", "1"])
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(value["records"].as_array().unwrap().len(), 1);
  assert_eq!(value["records"][0]["outcome"], "interrupted");
  assert_eq!(value["complete"], false);
  assert!(value["warning"].is_string());
  let output = fixture.command(&["audit", "--failed"]).output().unwrap();
  assert!(output.status.success());
  let table = String::from_utf8(output.stdout).unwrap();
  assert!(table.contains("connection"));
  assert!(table.contains("interrupted"));
  assert!(table.contains("123 ms"));
  assert!(!table.contains("succeeded"));
  assert!(!table.contains(&"a".repeat(64)));
  assert!(
    String::from_utf8(output.stderr)
      .unwrap()
      .contains("Warning:")
  );
}
