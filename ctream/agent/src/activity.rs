//! Host-owned attribution and arbitration for named desktop mutations.

use ctream_proto::{Target, valid_agent_id, valid_window_id};
use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use std::{
  fs::{self, DirBuilder, OpenOptions},
  io::{self, Write},
  path::{Path, PathBuf},
  time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_RECENT: usize = 1_000;
const RETAINED: i64 = 10_000;
const APPLICATION_ID: i32 = 0x4354_4143; // CTAC
const DATABASE: &str = "activity.sqlite3";

/// One agent-side session. Observations can share the session without opening
/// the database or contending with an active mutation.
pub struct Journal {
  base: PathBuf,
  session_id: String,
}

impl Journal {
  pub fn open() -> io::Result<Self> {
    Ok(Self::at(state_directory()?))
  }

  fn at(base: PathBuf) -> Self {
    Self {
      base,
      session_id: Uuid::new_v4().to_string(),
    }
  }

  pub fn session_id(&self) -> &str {
    &self.session_id
  }

  /// Commit an unknown outcome before dispatch. The file lock spans the two
  /// SQLite transactions and the complete operation, including its host ACK.
  /// Readers can see the intent while the driver runs. Process exit drops the
  /// lock; an interrupted or unconfirmed action retains its unknown outcome.
  pub fn record(
    &mut self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &'static str,
    apply: impl FnOnce() -> io::Result<()>,
  ) -> io::Result<()> {
    if !valid_agent_id(agent_id)
      || window_id.is_some_and(|id| !valid_window_id(id))
      || operation.is_empty()
      || operation.len() > 64
      || !operation
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b == b'_')
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid activity metadata",
      ));
    }
    let dir = prepare(&self.base, true)?;
    let lock = private_options()
      .create(true)
      .read(true)
      .write(true)
      .open(dir.join("activity.lock"))?;
    check(&lock.metadata()?, false, true)?;
    lock.lock()?;
    let mut db = open_db(&dir, true)?;
    let tx = db.transaction().map_err(io::Error::other)?;
    if !initialized(&tx)? {
      tx.execute_batch(
        "CREATE TABLE actions (
          action_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL, agent_id TEXT NOT NULL,
          timestamp_ms INTEGER NOT NULL, target TEXT NOT NULL, window_id TEXT, operation TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('unknown','failed','succeeded'))
        ) STRICT;"
      ).map_err(io::Error::other)?;
      tx.pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(io::Error::other)?;
      tx.pragma_update(None, "user_version", 1)
        .map_err(io::Error::other)?;
    }
    let action_id = Uuid::new_v4().to_string();
    let timestamp_ms: i64 = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_err(io::Error::other)?
      .as_millis()
      .try_into()
      .map_err(io::Error::other)?;
    tx.execute(
      "INSERT INTO actions(action_id,session_id,agent_id,timestamp_ms,target,window_id,operation,status)
       VALUES (?1,?2,?3,?4,?5,?6,?7,'unknown')",
      params![action_id,self.session_id,agent_id,timestamp_ms,target.to_string(),window_id,operation],
    ).map_err(io::Error::other)?;
    tx.execute(
      "DELETE FROM actions WHERE rowid <= ?1",
      [tx.last_insert_rowid() - RETAINED],
    )
    .map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    let result = apply();
    let status = if result.is_ok() {
      "succeeded"
    } else {
      "failed"
    };
    let updated = db.execute(
      "UPDATE actions SET status=?1 WHERE action_id=?2 AND status='unknown'",
      params![status, action_id],
    );
    if !matches!(updated, Ok(1)) {
      return Err(io::Error::other(
        "the action may already have run and its recorded outcome is unknown",
      ));
    }
    result
  }
}

fn state_directory() -> io::Result<PathBuf> {
  dirs::state_dir()
    .or_else(dirs::data_local_dir)
    .ok_or_else(|| io::Error::other("host user state directory is unavailable"))
}

fn check(metadata: &fs::Metadata, directory: bool, private: bool) -> io::Result<()> {
  let regular = if directory {
    metadata.is_dir()
  } else {
    metadata.is_file()
  };
  #[cfg(unix)]
  let permitted = {
    use std::os::unix::fs::MetadataExt;
    let forbidden = if private { 0o077 } else { 0o022 };
    metadata.uid() == rustix::process::getuid().as_raw()
      && metadata.mode() & forbidden == 0
      && (directory || metadata.nlink() == 1)
  };
  #[cfg(windows)]
  let permitted = {
    use std::os::windows::fs::MetadataExt;
    // LocalAppData supplies the user's inherited ACLs. Reject junctions and
    // other reparse points before following a path within that namespace.
    let _ = private;
    metadata.file_attributes() & 0x400 == 0
  };
  if regular && permitted {
    Ok(())
  } else {
    Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "unsafe host activity storage",
    ))
  }
}

fn prepare(base: &Path, create: bool) -> io::Result<PathBuf> {
  let mut builder = DirBuilder::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(0o700);
  }
  // The configured OS state root may not exist on the user's first run.
  if create {
    builder.recursive(true).create(base)?;
  }
  check(&fs::symlink_metadata(base)?, true, false)?;
  let mut dir = base.to_owned();
  for (component, private) in [("ctream", false), ("activity", true)] {
    dir.push(component);
    if create {
      match builder.recursive(false).create(&dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        result => result?,
      }
    }
    check(&fs::symlink_metadata(&dir)?, true, private)?;
  }
  dir.canonicalize()
}

fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    options.mode(0o600).custom_flags(flags.bits() as i32);
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  options
}

fn open_db(dir: &Path, write: bool) -> io::Result<Connection> {
  let path = dir.join(DATABASE);
  if write {
    // Never open and close an existing SQLite file behind SQLite's back. On
    // Unix that can release another connection's process-scoped byte locks.
    match private_options().create_new(true).write(true).open(&path) {
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      result => {
        result?;
      }
    }
  }
  check(&fs::symlink_metadata(&path)?, false, true)?;
  for suffix in ["-journal", "-wal", "-shm"] {
    match fs::symlink_metadata(dir.join(format!("{DATABASE}{suffix}"))) {
      Ok(metadata) => check(&metadata, false, true)?,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(error),
    }
  }
  let flags = if write {
    OpenFlags::SQLITE_OPEN_READ_WRITE
  } else {
    OpenFlags::SQLITE_OPEN_READ_ONLY
  };
  let db = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)
    .map_err(io::Error::other)?;
  db.busy_timeout(Duration::from_secs(2))
    .map_err(io::Error::other)?;
  db.pragma_update(None, "trusted_schema", false)
    .map_err(io::Error::other)?;
  if write {
    db.pragma_update(None, "synchronous", "FULL")
      .map_err(io::Error::other)?;
  }
  Ok(db)
}

fn initialized(db: &Connection) -> io::Result<bool> {
  let (id, version, objects): (i32, i32, i64) = db
    .query_row(
      "SELECT application_id, user_version, (SELECT COUNT(*) FROM sqlite_schema)
     FROM pragma_application_id, pragma_user_version",
      [],
      |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .map_err(io::Error::other)?;
  match (id, version, objects) {
    (0, 0, 0) => Ok(false),
    (APPLICATION_ID, 1, _) => Ok(true),
    _ => Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "unrecognized host activity database",
    )),
  }
}

#[derive(Serialize)]
struct Entry {
  action_id: String,
  session_id: String,
  agent_id: String,
  timestamp_ms: i64,
  target: String,
  window_id: Option<String>,
  operation: String,
  status: String,
}

fn recent(base: &Path, limit: usize) -> io::Result<Vec<Entry>> {
  if !(1..=MAX_RECENT).contains(&limit) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "activity limit must be between 1 and 1000",
    ));
  }
  let db = match prepare(base, false).and_then(|dir| open_db(&dir, false)) {
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
    result => result?,
  };
  if !initialized(&db)? {
    return Ok(Vec::new());
  }
  let mut query = db
    .prepare(
      "SELECT action_id,session_id,agent_id,timestamp_ms,target,window_id,operation,status
    FROM actions ORDER BY rowid DESC LIMIT ?1",
    )
    .map_err(io::Error::other)?;
  query
    .query_map([limit as i64], |row| {
      Ok(Entry {
        action_id: row.get(0)?,
        session_id: row.get(1)?,
        agent_id: row.get(2)?,
        timestamp_ms: row.get(3)?,
        target: row.get(4)?,
        window_id: row.get(5)?,
        operation: row.get(6)?,
        status: row.get(7)?,
      })
    })
    .map_err(io::Error::other)?
    .collect::<rusqlite::Result<_>>()
    .map_err(io::Error::other)
}

/// Write recent actions in host dispatch order, newest first. Missing history is empty.
pub fn write_recent(mut output: impl Write, limit: usize) -> io::Result<()> {
  serde_json::to_writer(&mut output, &recent(&state_directory()?, limit)?)?;
  output.write_all(b"\n")
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    fs::TryLockError,
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
  };

  struct Fixture(PathBuf);

  struct TestProcess(Child);

  impl Drop for TestProcess {
    fn drop(&mut self) {
      if !matches!(self.0.try_wait(), Ok(Some(_))) {
        let _ = self.0.kill();
        let _ = self.0.wait();
      }
    }
  }

  impl Fixture {
    fn new() -> Self {
      Self(std::env::temp_dir().join(format!("ctream-activity-test-{}", Uuid::new_v4())))
    }

    fn journal(&self) -> Journal {
      Journal::at(self.0.clone())
    }

    fn child(&self, mode: &str) -> TestProcess {
      TestProcess(
        Command::new(std::env::current_exe().unwrap())
          .args(["--exact", "activity::tests::subprocess", "--nocapture"])
          .env("CTREAM_ACTIVITY_TEST_BASE", &self.0)
          .env("CTREAM_ACTIVITY_TEST_MODE", mode)
          .stdout(Stdio::piped())
          .spawn()
          .unwrap(),
      )
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn subprocess() {
    let Some(base) = std::env::var_os("CTREAM_ACTIVITY_TEST_BASE") else {
      return;
    };
    let mut journal = Journal::at(base.into());
    match std::env::var("CTREAM_ACTIVITY_TEST_MODE").as_deref() {
      Ok("crash") => {
        journal
          .record("worker", Target::X11(0), None, "click", || {
            std::process::exit(42)
          })
          .unwrap();
      }
      Ok("contend") => {
        let dir = prepare(&journal.base, false).unwrap();
        let file = private_options()
          .read(true)
          .write(true)
          .open(dir.join("activity.lock"))
          .unwrap();
        assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));
        println!("parent holds lock");
        io::stdout().flush().unwrap();
        let base = journal.base.clone();
        journal
          .record("worker", Target::X11(0), None, "click", || {
            let rows = recent(&base, 2)?;
            assert_eq!(rows[0].status, "unknown");
            assert_eq!(rows[1].status, "succeeded");
            Ok(())
          })
          .unwrap();
      }
      mode => panic!("unexpected test mode {mode:?}"),
    }
  }

  #[test]
  fn independent_processes_serialize_complete_actions_and_allow_live_reads() {
    let fixture = Fixture::new();
    let mut journal = fixture.journal();
    let session_id = journal.session_id().to_owned();
    let mut process = None;
    journal
      .record(
        "first",
        Target::WindowsDesktop,
        Some("window-1"),
        "drag",
        || {
          let rows = recent(&fixture.0, 2)?;
          assert_eq!(rows.len(), 1);
          assert_eq!(rows[0].status, "unknown");
          let mut child = fixture.child("contend");
          let mut stdout = BufReader::new(child.0.stdout.take().unwrap());
          let mut line = String::new();
          while !line.ends_with("parent holds lock\n") {
            line.clear();
            assert_ne!(
              stdout.read_line(&mut line)?,
              0,
              "child exited before contending"
            );
          }
          process = Some((child, stdout));
          Ok(())
        },
      )
      .unwrap();
    let (mut child, _stdout) = process.unwrap();
    assert!(child.0.wait().unwrap().success());
    let rows = recent(&fixture.0, 2).unwrap();
    assert_eq!(
      (rows[0].agent_id.as_str(), rows[0].status.as_str()),
      ("worker", "succeeded")
    );
    assert_eq!(
      (rows[1].agent_id.as_str(), rows[1].target.as_str()),
      ("first", "windows")
    );
    assert_eq!(rows[1].window_id.as_deref(), Some("window-1"));
    assert_eq!(rows[1].session_id, session_id);
    assert_ne!(rows[1].session_id, rows[0].session_id);
    assert_ne!(rows[1].action_id, rows[0].action_id);
  }

  #[test]
  fn interrupted_action_stays_unknown_and_driver_failure_stays_redacted() {
    let fixture = Fixture::new();
    assert_eq!(fixture.child("crash").0.wait().unwrap().code(), Some(42));
    let mut journal = fixture.journal();
    let error = journal
      .record(
        "second",
        Target::Wayland(1),
        Some("w-2"),
        "set_text",
        || Err(io::Error::other("sensitive detail")),
      )
      .unwrap_err();
    assert_eq!(error.to_string(), "sensitive detail");
    let rows = recent(&fixture.0, 2).unwrap();
    assert_eq!(rows[0].status, "failed");
    assert_eq!(rows[1].status, "unknown");
    assert!(
      !serde_json::to_string(&rows)
        .unwrap()
        .contains("sensitive detail")
    );
    assert_eq!(recent(&fixture.0, 1).unwrap().len(), 1);
    assert!(recent(&fixture.0, MAX_RECENT + 1).is_err());
    let other = Fixture::new();
    assert!(recent(&other.0, 1).unwrap().is_empty());
    assert!(!other.0.exists());
  }

  #[test]
  fn outcome_write_error_keeps_the_committed_intent_unknown() {
    let fixture = Fixture::new();
    let error = fixture
      .journal()
      .record("worker", Target::X11(0), None, "click", || {
        let db = open_db(&prepare(&fixture.0, false)?, true)?;
        db.execute_batch(
          "CREATE TRIGGER fail_outcome BEFORE UPDATE ON actions
        BEGIN SELECT RAISE(FAIL,'simulated write failure'); END",
        )
        .map_err(io::Error::other)
      })
      .unwrap_err();
    assert!(error.to_string().contains("may already have run"));
    assert_eq!(recent(&fixture.0, 1).unwrap()[0].status, "unknown");
  }

  #[test]
  fn old_rows_are_pruned_when_the_next_intent_is_committed() {
    let fixture = Fixture::new();
    let mut journal = fixture.journal();
    journal
      .record("tester", Target::X11(1), None, "move", || Ok(()))
      .unwrap();
    let dir = prepare(&fixture.0, false).unwrap();
    let db = open_db(&dir, true).unwrap();
    db.execute("WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n < ?1)
      INSERT INTO actions SELECT 'seed-' || n,'worker','tester',1,':1',NULL,'move','succeeded' FROM seq",
      [RETAINED - 1]).unwrap();
    journal
      .record("tester", Target::X11(1), None, "move", || Ok(()))
      .unwrap();
    let count: i64 = db
      .query_row("SELECT count(*) FROM actions", [], |row| row.get(0))
      .unwrap();
    assert_eq!(count, RETAINED);
    let first: String = db
      .query_row(
        "SELECT action_id FROM actions ORDER BY rowid LIMIT 1",
        [],
        |row| row.get(0),
      )
      .unwrap();
    assert_eq!(first, "seed-1");
  }

  #[cfg(unix)]
  #[test]
  fn unsafe_permissions_block_dispatch() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let dir = prepare(&fixture.0, true).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    let error = fixture
      .journal()
      .record("tester", Target::X11(0), None, "click", || {
        panic!("dispatched")
      })
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
  }

  #[test]
  fn unrelated_database_blocks_dispatch_and_preserves_contents() {
    let fixture = Fixture::new();
    let db = open_db(&prepare(&fixture.0, true).unwrap(), true).unwrap();
    db.execute("CREATE TABLE existing(value TEXT)", []).unwrap();
    let error = fixture
      .journal()
      .record("tester", Target::X11(0), None, "click", || {
        panic!("dispatched")
      })
      .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
      db.query_row("SELECT name FROM sqlite_schema", [], |row| row
        .get::<_, String>(0))
        .unwrap(),
      "existing"
    );
  }
}
