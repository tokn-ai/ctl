//! Durable, redacted host decisions and outcomes for desktop input requests.

use ctream_proto::{Target, live::TurnError, valid_agent_id, valid_window_id};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde::Serialize;
use std::{
  fs::{self, DirBuilder, OpenOptions},
  io::{self, Write},
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
  time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_RECENT: usize = 1_000;
const RETAINED: i64 = 10_000;
const APPLICATION_ID: i32 = 0x4354_4143; // CTAC
const DATABASE: &str = "activity.sqlite3";

/// The driver explicitly confirmed delivery or failure, or delivery was uncertain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
  Succeeded,
  Failed,
  Unknown,
}

impl Outcome {
  pub fn from_result(result: &io::Result<()>) -> Self {
    match result {
      Ok(()) => Self::Succeeded,
      Err(error) if ctream_driver::UnconfirmedDelivery::is(error) => Self::Unknown,
      Err(_) => Self::Failed,
    }
  }

  fn as_str(self) -> &'static str {
    match self {
      Self::Succeeded => "succeeded",
      Self::Failed => "failed",
      Self::Unknown => "unknown",
    }
  }
}

struct Writer {
  db: Connection,
  account_id: String,
}

/// One lazy SQLite writer per display daemon. The mutex covers short database
/// transactions; device execution and acknowledgement happen outside it.
#[derive(Default)]
pub(crate) struct Store {
  base: Option<PathBuf>,
  writer: Mutex<Option<Writer>>,
}

impl Store {
  #[cfg(test)]
  pub(crate) fn at(base: PathBuf) -> Arc<Self> {
    Arc::new(Self {
      base: Some(base),
      ..Default::default()
    })
  }

  pub(crate) fn for_session(self: &Arc<Self>, participant_id: &str) -> io::Result<Journal> {
    if !valid_uuid(participant_id) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid activity participant ID",
      ));
    }
    Ok(Journal {
      store: self.clone(),
      session_id: participant_id.to_owned(),
      pending: None,
    })
  }

  fn with_writer<T>(&self, write: impl FnOnce(&mut Writer) -> io::Result<T>) -> io::Result<T> {
    let mut slot = self
      .writer
      .lock()
      .map_err(|_| io::Error::other("activity writer unavailable"))?;
    if slot.is_none() {
      let base = match &self.base {
        Some(base) => base.clone(),
        None => state_directory()?,
      };
      let dir = prepare(&base, true)?;
      let mut db = open_db(&dir, true)?;
      initialize_writer(&mut db)?;
      *slot = Some(Writer {
        db,
        account_id: host_account_id()?,
      });
    }
    write(slot.as_mut().expect("writer initialized"))
  }
}

/// Each controller issues one mutation at a time. Its outstanding receipt
/// records the insertion position in case completion arrives after retention.
pub(crate) struct Journal {
  store: Arc<Store>,
  session_id: String,
  pending: Option<(String, i64)>,
}

impl Journal {
  /// Commit an unknown outcome before the daemon sends a mutation to a worker.
  pub fn start(
    &mut self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &str,
    turn_ref: Option<&str>,
  ) -> io::Result<String> {
    if self.pending.is_some() {
      return Err(io::Error::other(
        "previous activity request is still in flight",
      ));
    }
    let (action_id, position) =
      self.insert(agent_id, target, window_id, operation, turn_ref, None)?;
    self.pending = Some((action_id.clone(), position));
    Ok(action_id)
  }

  /// Commit the admission decision, including the conflicting owner, atomically.
  /// Only known decision codes and public identifiers are persisted.
  pub fn refuse(
    &self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &str,
    turn_ref: Option<&str>,
    conflict: &TurnError,
  ) -> io::Result<String> {
    if !matches!(conflict.code.as_str(), "busy" | "stale_turn")
      || conflict
        .owner_id
        .as_deref()
        .is_some_and(|id| !valid_uuid(id))
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid activity refusal",
      ));
    }
    self
      .insert(
        agent_id,
        target,
        window_id,
        operation,
        turn_ref,
        Some(conflict),
      )
      .map(|(action_id, _)| action_id)
  }

  fn insert(
    &self,
    agent_id: &str,
    target: Target,
    window_id: Option<&str>,
    operation: &str,
    turn_ref: Option<&str>,
    refusal: Option<&TurnError>,
  ) -> io::Result<(String, i64)> {
    if !valid_agent_id(agent_id)
      || window_id.is_some_and(|id| !valid_window_id(id))
      || turn_ref.is_some_and(|id| !valid_uuid(id))
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
    let action_id = Uuid::new_v4().to_string();
    self.store.with_writer(|writer| {
      let tx = writer
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
      let timestamp_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis()
        .try_into()
        .map_err(io::Error::other)?;
      let status = if refusal.is_some() { "failed" } else { "unknown" };
      tx.execute(
        "INSERT INTO actions(action_id,session_id,agent_id,timestamp_ms,target,window_id,operation,status)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
          action_id, self.session_id, agent_id, timestamp_ms, target.to_string(),
          window_id, operation, status
        ],
      )
      .map_err(io::Error::other)?;
      let row_id = tx.last_insert_rowid();
      tx.execute(
        "INSERT INTO action_context(action_id,account_id,turn_ref) VALUES (?1,?2,?3)",
        params![action_id, writer.account_id, turn_ref],
      )
      .map_err(io::Error::other)?;
      if let Some(conflict) = refusal {
        tx.execute(
          "INSERT INTO action_refusals(action_id,refusal_code,owner_id) VALUES (?1,?2,?3)",
          params![action_id, conflict.code, conflict.owner_id],
        )
        .map_err(io::Error::other)?;
      }
      tx.execute(
        "DELETE FROM actions WHERE rowid <= ?1",
        [row_id - RETAINED],
      )
      .map_err(io::Error::other)?;
      tx.commit().map_err(io::Error::other)?;
      Ok((action_id, row_id))
    })
  }

  pub fn finish(&mut self, action_id: &str, outcome: Outcome) -> io::Result<()> {
    let (_, row_id) = self
      .pending
      .take_if(|(id, _)| id == action_id)
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "activity request is not pending in this session",
        )
      })?;
    self
      .store
      .with_writer(|writer| {
        let tx = writer
          .db
          .transaction_with_behavior(TransactionBehavior::Immediate)
          .map_err(io::Error::other)?;
        let updated = tx
          .execute(
            "UPDATE actions SET status=?1 WHERE action_id=?2 AND status='unknown'",
            params![outcome.as_str(), action_id],
          )
          .map_err(io::Error::other)?;
        if updated == 0 {
          let pruned: bool = tx
            .query_row(
              "SELECT COALESCE(MAX(rowid) >= ?1, 0) FROM actions",
              [row_id + RETAINED],
              |row| row.get(0),
            )
            .map_err(io::Error::other)?;
          if !pruned {
            return Err(io::Error::other(
              "activity request disappeared from history",
            ));
          }
        }
        tx.commit().map_err(io::Error::other)?;
        Ok(())
      })
      .map_err(|error| {
        ctream_driver::UnconfirmedDelivery::error(io::Error::other(format!(
          "the action may already have run and its recorded outcome is unknown: {error}"
        )))
      })
  }
}

fn valid_uuid(id: &str) -> bool {
  Uuid::try_parse(id).is_ok_and(|uuid| uuid.to_string() == id)
}

fn initialize_writer(db: &mut Connection) -> io::Result<()> {
  let tx = db
    .transaction_with_behavior(TransactionBehavior::Immediate)
    .map_err(io::Error::other)?;
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
  // Side tables keep the existing 'activity' rows and read-only clients compatible.
  tx.execute_batch(
    "CREATE TABLE IF NOT EXISTS action_context (
       action_id TEXT PRIMARY KEY, account_id TEXT NOT NULL, turn_ref TEXT
     ) STRICT;
     CREATE TRIGGER IF NOT EXISTS prune_action_context AFTER DELETE ON actions
     BEGIN DELETE FROM action_context WHERE action_id = OLD.action_id; END;
     CREATE TABLE IF NOT EXISTS action_refusals (
       action_id TEXT PRIMARY KEY, refusal_code TEXT NOT NULL
         CHECK(refusal_code IN ('busy','stale_turn')), owner_id TEXT
     ) STRICT;
     CREATE TRIGGER IF NOT EXISTS prune_action_refusals AFTER DELETE ON actions
     BEGIN DELETE FROM action_refusals WHERE action_id = OLD.action_id; END;",
  )
  .map_err(io::Error::other)?;
  tx.commit().map_err(io::Error::other)
}

#[cfg(unix)]
fn host_account_id() -> io::Result<String> {
  Ok(format!("uid:{}", rustix::process::geteuid().as_raw()))
}

#[cfg(windows)]
fn host_account_id() -> io::Result<String> {
  crate::daemon::host_account_id()
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
  #[serde(skip_serializing_if = "Option::is_none")]
  account_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  turn_ref: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  refusal_code: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  owner_id: Option<String>,
}

fn recent(base: &Path, limit: usize) -> io::Result<Vec<Entry>> {
  recent_with_context(base, limit, false)
}

fn recent_with_context(base: &Path, limit: usize, details: bool) -> io::Result<Vec<Entry>> {
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
  let (context, refusals) = if details {
    db.query_row(
      "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='action_context'),
              EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='action_refusals')",
      [],
      |row| Ok((row.get::<_, bool>(0)?, row.get::<_, bool>(1)?)),
    )
    .map_err(io::Error::other)?
  } else {
    (false, false)
  };
  let (account_fields, account_join) = if context {
    (
      "c.account_id,c.turn_ref",
      "LEFT JOIN action_context c ON c.action_id=a.action_id",
    )
  } else {
    ("NULL,NULL", "")
  };
  let (refusal_fields, refusal_join) = if refusals {
    (
      "r.refusal_code,r.owner_id",
      "LEFT JOIN action_refusals r ON r.action_id=a.action_id",
    )
  } else {
    ("NULL,NULL", "")
  };
  let sql = format!(
    "SELECT a.action_id,a.session_id,a.agent_id,a.timestamp_ms,a.target,a.window_id,a.operation,a.status,
            {account_fields},{refusal_fields} FROM actions a {account_join} {refusal_join}
     ORDER BY a.rowid DESC LIMIT ?1"
  );
  let mut query = db.prepare(&sql).map_err(io::Error::other)?;
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
        account_id: row.get(8)?,
        turn_ref: row.get(9)?,
        refusal_code: row.get(10)?,
        owner_id: row.get(11)?,
      })
    })
    .map_err(io::Error::other)?
    .collect::<rusqlite::Result<_>>()
    .map_err(io::Error::other)
}

pub fn write_recent_details(mut output: impl Write, limit: usize) -> io::Result<()> {
  serde_json::to_writer(
    &mut output,
    &recent_with_context(&state_directory()?, limit, true)?,
  )?;
  output.write_all(b"\n")
}

/// Write recent requests in host journal order, newest first. Missing history is empty.
pub fn write_recent(mut output: impl Write, limit: usize) -> io::Result<()> {
  serde_json::to_writer(&mut output, &recent(&state_directory()?, limit)?)?;
  output.write_all(b"\n")
}

#[cfg(test)]
mod tests;
