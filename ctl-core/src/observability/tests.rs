use super::*;
use std::fs;

struct Fixture(std::path::PathBuf);
impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctl-history-{}", Uuid::new_v4()));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      builder.mode(0o700);
    }
    builder.create(&root).unwrap();
    Self(root)
  }
  fn store(&self) -> Store {
    Store::new(self.0.join("history"))
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn record(outcome: Outcome) -> Record {
  let operation = Operation::start(Event::CredentialSave, Some("private-password-marker"));
  let mut record = operation.record.clone();
  record.outcome = outcome;
  record
}

#[test]
fn typed_records_hash_subjects_and_never_include_secret_bodies() {
  let record = record(Outcome::Succeeded);
  let json = serde_json::to_string(&record).unwrap();
  assert!(!json.contains("private-password-marker"));
  assert!(!json.contains("passphrase"));
  assert_eq!(record.subject_id.unwrap().len(), 64);
}

#[test]
fn missing_history_is_read_only_and_limits_are_validated() {
  let fixture = Fixture::new();
  let store = fixture.store();
  assert!(
    store
      .read(Stream::Audit, 100, false)
      .unwrap()
      .records
      .is_empty()
  );
  assert!(!store.directory().exists());
  assert!(store.read(Stream::Audit, 0, false).is_err());
  assert!(store.read(Stream::Audit, 10001, false).is_err());
}

#[test]
fn rotation_and_failure_filter_keep_newest_matching_records_in_append_order() {
  let fixture = Fixture::new();
  let store = Store::small(fixture.0.join("history"), 1000);
  let mut failures = Vec::new();
  for index in 0..12 {
    let record = record(if index % 2 == 0 {
      Outcome::Failed
    } else {
      Outcome::Succeeded
    });
    if index % 2 == 0 {
      failures.push(record.event_id);
    }
    store.append(Stream::Logs, &record).unwrap();
  }
  let history = store.read(Stream::Logs, 2, true).unwrap();
  assert!(history.complete);
  assert_eq!(
    history
      .records
      .iter()
      .map(|record| record.event_id)
      .collect::<Vec<_>>(),
    failures[failures.len() - 2..]
  );
  assert!(!store.directory().join("logs.4.jsonl").exists());
  assert!(store.directory().join("logs.3.jsonl").exists());
}

#[test]
fn a_partial_line_is_reported_and_does_not_swallow_the_next_record() {
  use std::io::Write as _;
  let fixture = Fixture::new();
  let store = fixture.store();
  store
    .append(Stream::Audit, &record(Outcome::Started))
    .unwrap();
  fs::OpenOptions::new()
    .append(true)
    .open(store.directory().join("audit.jsonl"))
    .unwrap()
    .write_all(b"{partial")
    .unwrap();
  assert!(!store.read(Stream::Audit, 100, false).unwrap().complete);
  store
    .append(Stream::Audit, &record(Outcome::Succeeded))
    .unwrap();
  let history = store.read(Stream::Audit, 100, false).unwrap();
  assert!(!history.complete);
  assert_eq!(history.records.len(), 2);
}

#[cfg(unix)]
#[test]
fn symlinks_hardlinks_public_files_and_fifo_are_rejected_without_modifying_the_target() {
  use std::os::unix::fs::{PermissionsExt as _, symlink};
  let fixture = Fixture::new();
  let store = fixture.store();
  store
    .append(Stream::Audit, &record(Outcome::Succeeded))
    .unwrap();
  let path = store.directory().join("audit.jsonl");
  let original = fs::read(&path).unwrap();
  let protected = fixture.0.join("protected");
  fs::rename(&path, &protected).unwrap();
  symlink(&protected, &path).unwrap();
  assert!(
    store
      .append(Stream::Audit, &record(Outcome::Succeeded))
      .is_err()
  );
  assert_eq!(fs::read(&protected).unwrap(), original);
  assert!(store.read(Stream::Audit, 100, false).is_err());
  fs::remove_file(&path).unwrap();
  fs::hard_link(&protected, &path).unwrap();
  assert!(store.read(Stream::Audit, 100, false).is_err());
  fs::remove_file(&path).unwrap();
  fs::rename(&protected, &path).unwrap();
  fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
  assert!(store.read(Stream::Audit, 100, false).is_err());
  fs::remove_file(&path).unwrap();
  #[cfg(target_os = "linux")]
  rustix::fs::mkfifoat(
    rustix::fs::CWD,
    &path,
    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
  )
  .unwrap();
  #[cfg(not(target_os = "linux"))]
  fs::create_dir(&path).unwrap();
  assert!(store.read(Stream::Audit, 100, false).is_err());
}

#[test]
fn process_writer_child() {
  let Some(directory) = std::env::var_os("CTL_HISTORY_WRITER_TEST") else {
    return;
  };
  let store = Store::new(directory.into());
  for _ in 0..10 {
    store
      .append(Stream::Audit, &record(Outcome::Succeeded))
      .unwrap();
  }
}

#[test]
fn multiple_processes_append_complete_records_without_losing_events() {
  let _process_guard = crate::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let store = fixture.store();
  let mut children = Vec::new();
  for _ in 0..4 {
    children.push(
      std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "observability::tests::process_writer_child"])
        .env("CTL_HISTORY_WRITER_TEST", store.directory())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap(),
    );
  }
  // Reap every writer before checking results, and preserve child failures
  // instead of discarding the only useful diagnostic on Windows.
  let outputs: Vec<_> = children
    .into_iter()
    .map(|child| child.wait_with_output().unwrap())
    .collect();
  for output in outputs {
    assert!(output.status.success(), "{output:?}");
  }
  let history = store.read(Stream::Audit, 100, false).unwrap();
  assert!(history.complete);
  assert_eq!(history.records.len(), 40);
  let ids: std::collections::HashSet<_> = history
    .records
    .iter()
    .map(|record| record.event_id)
    .collect();
  assert_eq!(ids.len(), 40);
}

#[test]
fn store_lock_contention_is_bounded_and_oversized_files_are_refused() {
  use std::io::Write as _;
  let fixture = Fixture::new();
  let store = Store::small(fixture.0.join("history"), 1000);
  let record = record(Outcome::Succeeded);
  store.append(Stream::Logs, &record).unwrap();
  let lock = fs::File::open(store.directory().join("history.lock")).unwrap();
  lock.lock().unwrap();
  let error = store.append(Stream::Logs, &record).unwrap_err();
  assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
  lock.unlock().unwrap();
  let path = store.directory().join("logs.jsonl");
  fs::OpenOptions::new()
    .append(true)
    .open(&path)
    .unwrap()
    .write_all(&vec![b'x'; 1001])
    .unwrap();
  let original = fs::read(&path).unwrap();
  assert!(store.append(Stream::Logs, &record).is_err());
  assert!(store.read(Stream::Logs, 100, false).is_err());
  assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn operation_child() {
  if std::env::var_os("CTL_HISTORY_OPERATION_TEST").is_none() {
    return;
  }
  initialize();
  drop(Operation::start(
    Event::Connection,
    Some("private-connection-canary"),
  ));
  Operation::diagnostic(Event::DaemonLifecycle).finish(Outcome::Succeeded, None, None);
}

#[cfg(unix)]
#[test]
fn dropped_operations_are_interrupted_and_diagnostics_are_not_audit_events() {
  let _process_guard = crate::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let output = std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "observability::tests::operation_child"])
    .env("CTL_HISTORY_OPERATION_TEST", "1")
    .env("HOME", &fixture.0)
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let store = Store::new(fixture.0.join(".tokn/ctl/history"));
  let audit = store.read(Stream::Audit, 100, false).unwrap();
  assert_eq!(audit.records.len(), 2);
  assert_eq!(audit.records[0].outcome, Outcome::Started);
  assert_eq!(audit.records[1].outcome, Outcome::Interrupted);
  assert_eq!(audit.records[0].operation_id, audit.records[1].operation_id);
  assert_eq!(
    store.read(Stream::Logs, 100, false).unwrap().records.len(),
    4
  );
}
