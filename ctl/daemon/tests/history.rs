#![cfg(unix)]
use ctl_core::observability::{Event, Outcome, Store, Stream};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctld-history-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    Self(path)
  }
  fn command(&self) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctld"));
    command
      .env("HOME", &self.0)
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS");
    command
  }
  fn store(&self) -> Store {
    Store::new(self.0.join(".tokn/ctl/history"))
  }
  fn request(&self) -> std::process::Output {
    let mut child = self
      .command()
      .arg("--credential-request")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap();
    // Invalid identifier is rejected before any Keychain access, on every OS.
    child
      .stdin
      .take()
      .unwrap()
      .write_all(br#"{"type":"forget","credential_id":"synthetic-private-canary"}"#)
      .unwrap();
    child.wait_with_output().unwrap()
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[test]
fn failed_credential_request_records_correlated_redacted_audit_without_changing_json_stdout() {
  let fixture = Fixture::new();
  let output = fixture.request();
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  let response: ctl_ipc::credentials::Response = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    matches!(response, ctl_ipc::credentials::Response::Error { code, .. } if code == "credential_request_invalid")
  );
  let history = fixture.store().read(Stream::Audit, 100, false).unwrap();
  assert!(history.complete);
  assert_eq!(history.records.len(), 2);
  assert_eq!(
    history.records[0].operation_id,
    history.records[1].operation_id
  );
  assert_ne!(history.records[0].event_id, history.records[1].event_id);
  assert_eq!(history.records[0].event, Event::CredentialRemove);
  assert_eq!(history.records[0].outcome, Outcome::Started);
  assert_eq!(history.records[1].outcome, Outcome::Failed);
  let run = history.records[0].run_id;
  assert_eq!(run, history.records[1].run_id);
  for path in [
    fixture.store().directory().join("audit.sqlite3"),
    fixture
      .store()
      .directory()
      .join("logs")
      .join(format!("{run}.log")),
  ] {
    assert!(
      !fs::read(&path)
        .unwrap()
        .windows(b"synthetic-private-canary".len())
        .any(|bytes| bytes == b"synthetic-private-canary")
    );
    assert_eq!(
      fs::metadata(path).unwrap().permissions().mode() & 0o777,
      0o600
    );
  }
  let output = fixture.request();
  assert!(output.status.success(), "{output:?}");
  let history = fixture.store().read(Stream::Audit, 100, false).unwrap();
  assert_eq!(history.records.len(), 4);
  assert_ne!(history.records[2].run_id, run);
  assert_eq!(
    fixture
      .store()
      .read(Stream::Logs, 100, false)
      .unwrap()
      .records
      .len(),
    8
  );
}

#[test]
fn unsafe_history_path_warns_but_preserves_helper_response_and_target_file() {
  let fixture = Fixture::new();
  fs::create_dir_all(fixture.0.join(".tokn/ctl")).unwrap();
  let target = fixture.0.join("must-not-change");
  fs::write(&target, b"unchanged").unwrap();
  symlink(&target, fixture.store().directory()).unwrap();
  let output = fixture.request();
  assert!(output.status.success());
  assert!(serde_json::from_slice::<ctl_ipc::credentials::Response>(&output.stdout).is_ok());
  assert!(
    String::from_utf8(output.stderr)
      .unwrap()
      .contains("logging/audit recording failed")
  );
  assert_eq!(fs::read(target).unwrap(), b"unchanged");
}

#[test]
fn metadata_commands_do_not_create_history() {
  let fixture = Fixture::new();
  for arg in ["--component-info", "--protocol-build", "--protocol-version"] {
    let output = fixture.command().arg(arg).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stderr, Vec::<u8>::new());
  }
  assert!(fs::read_dir(&fixture.0).unwrap().next().is_none());
}

#[test]
fn proxy_argument_failure_is_saved_with_a_safe_message_and_no_raw_route() {
  let fixture = Fixture::new();
  let output = fixture
    .command()
    .env_remove("CTL_LOG_LEVEL")
    .args(["--proxy-route", "private-route-canary"])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(2));
  assert_eq!(output.stdout, Vec::<u8>::new());
  let history = fixture.store().read(Stream::Logs, 100, false).unwrap();
  assert!(history.complete);
  assert_eq!(history.records.len(), 2);
  let failure = &history.records[1];
  assert_eq!(failure.event, Event::ProxyConnection);
  assert_eq!(failure.outcome, Outcome::Failed);
  assert_eq!(
    failure.error_code.as_deref(),
    Some("proxy_destination_missing")
  );
  assert_eq!(
    failure.message(),
    "Proxy connection: failed; proxy host and port are required"
  );
  assert!(
    !serde_json::to_string(&history)
      .unwrap()
      .contains("private-route-canary")
  );
  assert!(!fixture.store().directory().join("audit.sqlite3").exists());
}
