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
    let mut record = record(if index % 2 == 0 {
      Outcome::Failed
    } else {
      Outcome::Succeeded
    });
    record.timestamp_ms = index;
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
  assert!(!store.log_path(run_id(), 4).exists());
  assert!(store.log_path(run_id(), 3).exists());
}

#[test]
fn a_partial_line_is_reported_and_does_not_swallow_the_next_record() {
  use std::io::Write as _;
  let fixture = Fixture::new();
  let store = fixture.store();
  store
    .append(Stream::Logs, &record(Outcome::Started))
    .unwrap();
  fs::OpenOptions::new()
    .append(true)
    .open(store.log_path(run_id(), 0))
    .unwrap()
    .write_all(b"{partial")
    .unwrap();
  assert!(!store.read(Stream::Logs, 100, false).unwrap().complete);
  store
    .append(Stream::Logs, &record(Outcome::Succeeded))
    .unwrap();
  let history = store.read(Stream::Logs, 100, false).unwrap();
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
  let path = store.directory().join("audit.sqlite3");
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
    let record = record(Outcome::Succeeded);
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    // A busy store is an expected, bounded result under concurrent fsyncs.
    // Retry only before-write contention; all other failures remain fatal.
    loop {
      match store.append(Stream::Audit, &record) {
        Ok(()) => break,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {}
        Err(error) => panic!("history writer failed: {error}"),
      }
    }
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
  let lock = fs::File::open(
    store
      .directory()
      .join("logs")
      .join(format!("{}.lock", run_id())),
  )
  .unwrap();
  lock.lock().unwrap();
  let error = store.append(Stream::Logs, &record).unwrap_err();
  assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
  lock.unlock().unwrap();
  let path = store.log_path(run_id(), 0);
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

#[test]
fn sqlite_contention_is_bounded_and_readers_do_not_mutate_history() {
  let fixture = Fixture::new();
  let store = fixture.store();
  let record = record(Outcome::Failed);
  store.append(Stream::Audit, &record).unwrap();
  let path = store.directory().join("audit.sqlite3");
  let original = fs::read(&path).unwrap();
  let connection = rusqlite::Connection::open(&path).unwrap();
  connection.execute_batch("BEGIN IMMEDIATE").unwrap();
  let started = Instant::now();
  let error = store.append(Stream::Audit, &record).unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert!(started.elapsed() < std::time::Duration::from_secs(5));
  // A reserved write lock still allows a read-only query.
  assert_eq!(store.read(Stream::Audit, 1, true).unwrap().records.len(), 1);
  connection.execute_batch("ROLLBACK").unwrap();
  assert_eq!(fs::read(&path).unwrap(), original);
  assert!(!store.directory().join("audit.sqlite3-wal").exists());
  assert!(!store.directory().join("audit.sqlite3-journal").exists());
}

#[test]
fn sqlite_filters_runs_preserves_append_order_and_reports_bad_records() {
  let fixture = Fixture::new();
  let store = fixture.store();
  let first_run = Uuid::new_v4();
  for (run, outcome) in [
    (first_run, Outcome::Failed),
    (Uuid::new_v4(), Outcome::Interrupted),
    (first_run, Outcome::Succeeded),
  ] {
    let mut record = record(outcome);
    record.run_id = run;
    store.append(Stream::Audit, &record).unwrap();
  }
  let history = store
    .read_run(Stream::Audit, 1, true, Some(first_run))
    .unwrap();
  assert!(history.complete);
  assert_eq!(history.records.len(), 1);
  assert_eq!(history.records[0].outcome, Outcome::Failed);
  let path = store.directory().join("audit.sqlite3");
  let connection = rusqlite::Connection::open(&path).unwrap();
  connection
    .execute(
      "UPDATE events SET record = '{}' WHERE outcome = 'succeeded'",
      [],
    )
    .unwrap();
  let history = store.read(Stream::Audit, 10, false).unwrap();
  assert!(!history.complete);
  assert_eq!(history.records.len(), 2);
  connection.pragma_update(None, "user_version", 99).unwrap();
  let original = fs::read(&path).unwrap();
  assert!(store.read(Stream::Audit, 10, false).is_err());
  assert!(
    store
      .append(Stream::Audit, &record(Outcome::Succeeded))
      .is_err()
  );
  assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn diagnostic_writers_and_rotation_are_isolated_by_run() {
  let fixture = Fixture::new();
  let store = Store::small(fixture.0.join("history"), 1000);
  let mut first = record(Outcome::Succeeded);
  first.run_id = Uuid::new_v4();
  first.timestamp_ms = 1;
  store.append(Stream::Logs, &first).unwrap();
  let original = fs::read(store.log_path(first.run_id, 0)).unwrap();
  let lock = fs::File::open(
    store
      .directory()
      .join("logs")
      .join(format!("{}.lock", first.run_id)),
  )
  .unwrap();
  lock.lock().unwrap();
  let mut second = record(Outcome::Failed);
  second.run_id = Uuid::new_v4();
  for index in 2..10 {
    second.timestamp_ms = index;
    second.event_id = Uuid::new_v4();
    store.append(Stream::Logs, &second).unwrap();
  }
  assert_eq!(fs::read(store.log_path(first.run_id, 0)).unwrap(), original);
  lock.unlock().unwrap();
  let selected = store
    .read_run(Stream::Logs, 10, false, Some(first.run_id))
    .unwrap();
  assert_eq!(selected.records.len(), 1);
  let combined = store.read(Stream::Logs, 1, true).unwrap();
  assert_eq!(combined.records[0].run_id, second.run_id);
  assert_eq!(combined.records[0].timestamp_ms, 9);
}

#[test]
fn sqlite_rejects_unsafe_sidecars_and_preserves_corrupt_databases() {
  let fixture = Fixture::new();
  let store = fixture.store();
  store
    .append(Stream::Audit, &record(Outcome::Succeeded))
    .unwrap();
  let path = store.directory().join("audit.sqlite3");
  let sidecar = store.directory().join("audit.sqlite3-journal");
  fs::create_dir(&sidecar).unwrap();
  assert!(
    store
      .append(Stream::Audit, &record(Outcome::Succeeded))
      .is_err()
  );
  assert!(store.read(Stream::Audit, 10, false).is_err());
  fs::remove_dir(&sidecar).unwrap();
  fs::write(&path, b"corrupt database").unwrap();
  assert!(
    store
      .append(Stream::Audit, &record(Outcome::Succeeded))
      .is_err()
  );
  assert!(store.read(Stream::Audit, 10, false).is_err());
  assert_eq!(fs::read(&path).unwrap(), b"corrupt database");
}

#[test]
fn equal_log_timestamps_preserve_append_order_and_latest_selection() {
  let fixture = Fixture::new();
  let store = Store::small(fixture.0.join("history"), 1000);
  let mut ids = Vec::new();
  for _ in 0..8 {
    let mut record = record(Outcome::Succeeded);
    record.timestamp_ms = 0;
    ids.push(record.event_id);
    store.append(Stream::Logs, &record).unwrap();
  }
  let history = store.read(Stream::Logs, 2, false).unwrap();
  assert_eq!(
    history
      .records
      .iter()
      .map(|record| record.event_id)
      .collect::<Vec<_>>(),
    ids[6..]
  );
}

#[test]
fn legacy_history_is_reported_and_left_untouched() {
  let fixture = Fixture::new();
  let store = fixture.store();
  fs::create_dir(store.directory()).unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o700)).unwrap();
  }
  for (stream, name) in [
    (Stream::Audit, "audit.1.jsonl"),
    (Stream::Logs, "logs.jsonl"),
  ] {
    let path = store.directory().join(name);
    fs::write(&path, b"old evidence").unwrap();
    let history = store.read(stream, 100, false).unwrap();
    assert!(history.records.is_empty());
    assert!(!history.complete);
    assert!(history.warning.unwrap().contains("Legacy"));
    assert_eq!(fs::read(path).unwrap(), b"old evidence");
  }
}

#[test]
fn uncommitted_audit_writer_child() {
  use std::io::{Read as _, Write as _};
  let Some(directory) = std::env::var_os("CTL_AUDIT_CRASH_TEST") else {
    return;
  };
  let directory = std::path::PathBuf::from(directory);
  let connection = rusqlite::Connection::open(directory.join("audit.sqlite3")).unwrap();
  connection
    .execute_batch("BEGIN IMMEDIATE; UPDATE events SET record = '{}'")
    .unwrap();
  // Force dirty pages out before signaling readiness, leaving a hot rollback
  // journal if the parent kills us. No commit is allowed in this child.
  connection.cache_flush().unwrap();
  std::fs::File::create(directory.join("writer-ready"))
    .unwrap()
    .write_all(b"ready")
    .unwrap();
  let mut byte = [0];
  std::io::stdin().read_exact(&mut byte).unwrap();
  panic!("the parent must terminate the uncommitted writer");
}

#[test]
fn killed_audit_writer_recovers_without_publishing_uncommitted_events() {
  let _process_guard = crate::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let store = fixture.store();
  let committed = record(Outcome::Succeeded);
  store.append(Stream::Audit, &committed).unwrap();
  let mut child = std::process::Command::new(std::env::current_exe().unwrap())
    .args([
      "--exact",
      "observability::tests::uncommitted_audit_writer_child",
    ])
    .env("CTL_AUDIT_CRASH_TEST", store.directory())
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let ready = store.directory().join("writer-ready");
  let deadline = Instant::now() + std::time::Duration::from_secs(10);
  while !ready.exists() && Instant::now() < deadline {
    if child.try_wait().unwrap().is_some() {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  let was_ready = ready.exists();
  let _ = child.kill();
  let output = child.wait_with_output().unwrap();
  assert!(
    was_ready,
    "uncommitted writer never became ready: {output:?}"
  );
  // A write-capable connection performs SQLite's rollback recovery. A read-only
  // CLI must report recovery-required instead of mutating this hot journal.
  store
    .append(Stream::Audit, &record(Outcome::Failed))
    .unwrap();
  let history = store.read(Stream::Audit, 100, false).unwrap();
  assert!(history.complete);
  assert_eq!(history.records.len(), 2);
  assert_eq!(history.records[0].event_id, committed.event_id);
  assert_eq!(history.records[1].outcome, Outcome::Failed);
}

#[test]
fn human_logs_round_trip_and_reject_corrupt_metadata() {
  let mut record = record(Outcome::Failed);
  record.level = Level::Warn;
  record.component = Component::Ctmuxd;
  record.context.session_id = Some(Uuid::new_v4());
  record.context.pane_id = Some(Uuid::new_v4());
  record.context.exit_code = Some(7);
  record.error_code = Some("pane_spawn_failed".into());
  let line = format!("{}\n", record.log_line().unwrap());
  assert!(line.contains("Z WARN ctmuxd"));
  assert!(!line.contains("private-password-marker"));
  let decoded = Record::from_log_line(line.as_bytes()).unwrap();
  assert_eq!(
    serde_json::to_value(decoded).unwrap(),
    serde_json::to_value(&record).unwrap()
  );
  assert!(Record::from_log_line(line.trim_end().as_bytes()).is_none());
  assert!(Record::from_log_line(line.replace("schema=3", "schema=99").as_bytes()).is_none());
  assert!(
    Record::from_log_line(line.replace("pane_spawn_failed", "secret/body").as_bytes()).is_none()
  );
}

#[test]
fn level_filters_select_latest_matching_records_before_limit() {
  let fixture = Fixture::new();
  let store = fixture.store();
  for (index, level) in [Level::Error, Level::Info, Level::Warn, Level::Debug]
    .into_iter()
    .enumerate()
  {
    let mut record = record(Outcome::Succeeded);
    record.timestamp_ms = index as u64;
    record.level = level;
    store.append(Stream::Logs, &record).unwrap();
  }
  let history = store
    .read_filtered(Stream::Logs, 1, false, None, Some(Level::Warn))
    .unwrap();
  assert_eq!(history.records.len(), 1);
  assert_eq!(history.records[0].level, Level::Warn);
}

#[test]
fn level_child() {
  if std::env::var_os("CTL_HISTORY_LEVEL_TEST").is_none() {
    return;
  }
  initialize();
  Operation::start(Event::CredentialRead, Some("credential-secret-canary")).finish(
    Outcome::Succeeded,
    None,
    None,
  );
  Operation::diagnostic_at(Event::SessionTransport, Level::Trace, Context::default()).finish(
    Outcome::Failed,
    Some("transport_failed"),
    None,
  );
  Operation::diagnostic_at(Event::PaneResize, Level::Debug, Context::default()).finish(
    Outcome::Succeeded,
    None,
    None,
  );
}

#[cfg(unix)]
#[test]
fn diagnostic_level_filter_never_suppresses_audit_and_keeps_escalated_errors() {
  let _process_guard = crate::test_fixtures::ProcessGuard::acquire_blocking();
  let fixture = Fixture::new();
  let output = std::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "observability::tests::level_child"])
    .env("CTL_HISTORY_LEVEL_TEST", "1")
    .env("CTL_LOG_LEVEL", "error")
    .env("HOME", &fixture.0)
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert_eq!(output.stderr, Vec::<u8>::new());
  let store = Store::new(fixture.0.join(".tokn/ctl/history"));
  assert_eq!(
    store.read(Stream::Audit, 100, false).unwrap().records.len(),
    2
  );
  let logs = store.read(Stream::Logs, 100, false).unwrap();
  assert_eq!(logs.records.len(), 1);
  assert_eq!(logs.records[0].event, Event::SessionTransport);
  assert_eq!(logs.records[0].level, Level::Error);
}
