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
    vec!["logs", "--level", "nope"],
    vec!["logs", "--level", "warn", "--path"],
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
  let logs = directory.join("logs");
  fs::create_dir(&logs).unwrap();
  fs::set_permissions(&logs, fs::Permissions::from_mode(0o700)).unwrap();
  let run_id = uuid::Uuid::new_v4();
  let mut content = String::new();
  for (timestamp, outcome) in ["failed", "succeeded", "interrupted"]
    .into_iter()
    .enumerate()
  {
    let record = serde_json::json!({
      "schema_version": 2, "run_id": run_id, "event_id": uuid::Uuid::new_v4(),
      "operation_id": uuid::Uuid::new_v4(), "timestamp_ms": timestamp, "process_id": 42,
      "event": "connection", "outcome": outcome, "subject_id": "a".repeat(64),
      "elapsed_ms": 123, "error_code": null, "os_error": null
    });
    let record: ctl_core::observability::Record = serde_json::from_value(record).unwrap();
    writeln!(content, "{}", record.log_line().unwrap()).unwrap();
  }
  content.push_str("{bad record}\n");
  for (name, bytes) in [
    (format!("{run_id}.lock"), ""),
    (format!("{run_id}.log"), content.as_str()),
  ] {
    let path = logs.join(name);
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
  }
  let output = fixture
    .command(&[
      "logs",
      "--json",
      "--failed",
      "--limit",
      "1",
      "--run",
      &run_id.to_string(),
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(value["records"].as_array().unwrap().len(), 1);
  assert_eq!(value["records"][0]["outcome"], "interrupted");
  assert_eq!(
    value["records"][0]["message"],
    "Host connection: interrupted"
  );
  assert_eq!(value["complete"], false);
  assert!(value["warning"].is_string());
  let output = fixture.command(&["logs", "--failed"]).output().unwrap();
  assert!(output.status.success());
  let table = String::from_utf8(output.stdout).unwrap();
  assert!(table.contains("connection"));
  assert!(table.contains("MESSAGE"));
  assert!(table.contains("Host connection: interrupted"));
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

#[cfg(unix)]
#[test]
fn sqlite_audit_reads_filter_runs_without_starting_daemon_or_mutating_database() {
  use std::os::unix::fs::PermissionsExt as _;
  let fixture = Fixture::new();
  let directory = fixture.0.join(".tokn/ctl/history");
  fs::create_dir_all(&directory).unwrap();
  fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
  let path = directory.join("audit.sqlite3");
  let connection = rusqlite::Connection::open(&path).unwrap();
  connection
    .execute_batch(
      "PRAGMA application_id = 1129598017; PRAGMA user_version = 1;
     CREATE TABLE events(sequence INTEGER PRIMARY KEY, event_id TEXT, run_id TEXT,
       timestamp_ms INTEGER, subject_id TEXT, outcome TEXT, record TEXT);",
    )
    .unwrap();
  let selected_run = uuid::Uuid::new_v4();
  for (run_id, outcome) in [
    (selected_run, "failed"),
    (uuid::Uuid::new_v4(), "interrupted"),
  ] {
    let event_id = uuid::Uuid::new_v4();
    let record = serde_json::json!({
      "schema_version": 2, "run_id": run_id, "event_id": event_id,
      "operation_id": uuid::Uuid::new_v4(), "timestamp_ms": 0, "process_id": 42,
      "event": "connection", "outcome": outcome, "subject_id": "a".repeat(64),
      "elapsed_ms": 123, "error_code": null, "os_error": null
    });
    connection.execute(
      "INSERT INTO events(event_id, run_id, timestamp_ms, subject_id, outcome, record) VALUES (?1, ?2, 0, ?3, ?4, ?5)",
      rusqlite::params![event_id.to_string(), run_id.to_string(), "a".repeat(64), outcome, record.to_string()],
    ).unwrap();
  }
  drop(connection);
  fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
  let before = fs::read(&path).unwrap();
  let output = fixture
    .command(&[
      "audit",
      "--json",
      "--failed",
      "--run",
      &selected_run.to_string(),
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(value["records"].as_array().unwrap().len(), 1);
  assert_eq!(value["records"][0]["outcome"], "failed");
  assert_eq!(value["complete"], true);
  assert_eq!(output.stderr, Vec::<u8>::new());
  let output = fixture
    .command(&["audit", "--run", &selected_run.to_string()])
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let table = String::from_utf8(output.stdout).unwrap();
  assert!(table.contains(&selected_run.to_string()[..8]));
  assert!(!table.contains(&selected_run.to_string()));
  assert_eq!(fs::read(&path).unwrap(), before);
  assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
}
