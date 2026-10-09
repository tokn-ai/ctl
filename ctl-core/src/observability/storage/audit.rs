use super::{History, MAX_RECORD_BYTES, Record, Store, exists, open};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use std::io;
use std::time::Duration;
use uuid::Uuid;

const APPLICATION_ID: i32 = 0x4354_4c41;
const SCHEMA_VERSION: i32 = 1;

impl Store {
  fn database(&self, write: bool) -> io::Result<Connection> {
    let path = self.directory.join("audit.sqlite3");
    // SQLite opens its own descriptors. Check the database and every possible
    // sidecar first; the private directory prevents other users substituting them.
    let _checked = open(&path, write)?;
    for suffix in ["-journal", "-wal", "-shm"] {
      let sidecar = self.directory.join(format!("audit.sqlite3{suffix}"));
      if exists(&sidecar)? {
        match open(&sidecar, false) {
          Ok(_checked) => {}
          // Another SQLite writer may finish and remove its journal after the
          // existence check, or after open but before metadata validation.
          Err(error) if error.kind() == io::ErrorKind::NotFound => {}
          Err(error) => return Err(error),
        }
      }
    }
    let flags = if write {
      OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
      OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection =
      Connection::open_with_flags(self.directory.canonicalize()?.join("audit.sqlite3"), flags)
        .map_err(|error| sql_error(&error))?;
    connection
      .busy_timeout(Duration::from_millis(250))
      .map_err(|error| sql_error(&error))?;
    // Refuse database-defined triggers/views that invoke unsafe functions.
    connection
      .pragma_update(None, "trusted_schema", false)
      .map_err(|error| sql_error(&error))?;
    Ok(connection)
  }

  pub(super) fn append_audit(&self, record: &Record) -> io::Result<()> {
    let mut connection = self.database(true)?;
    // Rollback journaling keeps read-only CLI queries from creating WAL sidecars.
    // FULL synchronous commits make each successful insert durable.
    connection
      .pragma_update(None, "synchronous", "FULL")
      .map_err(|error| sql_error(&error))?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|error| sql_error(&error))?;
    let application: i32 = transaction
      .pragma_query_value(None, "application_id", |row| row.get(0))
      .map_err(|error| sql_error(&error))?;
    let version: i32 = transaction
      .pragma_query_value(None, "user_version", |row| row.get(0))
      .map_err(|error| sql_error(&error))?;
    if application == 0 && version == 0 {
      let tables: i64 = transaction
        .query_row(
          "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
          [],
          |row| row.get(0),
        )
        .map_err(|error| sql_error(&error))?;
      if tables != 0 {
        return Err(io::Error::other("unrecognized audit database"));
      }
      transaction
        .execute_batch(
          "CREATE TABLE events (
          sequence INTEGER PRIMARY KEY,
          event_id TEXT NOT NULL UNIQUE,
          run_id TEXT NOT NULL,
          timestamp_ms INTEGER NOT NULL,
          subject_id TEXT,
          outcome TEXT NOT NULL,
          record TEXT NOT NULL CHECK(length(record) <= 2048)
        ) STRICT;
        CREATE INDEX events_time ON events(timestamp_ms);
        CREATE INDEX events_run ON events(run_id, sequence);
        CREATE INDEX events_subject ON events(subject_id, sequence);
        CREATE INDEX events_outcome ON events(outcome, sequence);",
        )
        .map_err(|error| sql_error(&error))?;
      transaction
        .pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(|error| sql_error(&error))?;
      transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| sql_error(&error))?;
    } else {
      check_schema(&transaction)?;
    }
    let encoded = serde_json::to_string(record).map_err(io::Error::other)?;
    if encoded.len() > MAX_RECORD_BYTES {
      return Err(io::Error::other("history record exceeds size limit"));
    }
    let outcome = enum_text(record.outcome)?;
    transaction
      .execute(
        "INSERT INTO events(event_id, run_id, timestamp_ms, subject_id, outcome, record)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
          record.event_id.to_string(),
          record.run_id.to_string(),
          i64::try_from(record.timestamp_ms).map_err(io::Error::other)?,
          record.subject_id,
          outcome,
          encoded
        ],
      )
      .map_err(|error| sql_error(&error))?;
    transaction.commit().map_err(|error| sql_error(&error))
  }

  pub(super) fn read_audit(
    &self,
    limit: usize,
    failed_only: bool,
    run: Option<Uuid>,
    level: Option<super::Level>,
  ) -> io::Result<History> {
    let mut history = History {
      records: Vec::new(),
      complete: true,
      warning: None,
    };
    // This storage format replaces the unreleased JSONL implementation. Preserve
    // old files and report them explicitly instead of silently hiding evidence.
    for index in 0..16 {
      let name = if index == 0 {
        "audit.jsonl".into()
      } else {
        format!("audit.{index}.jsonl")
      };
      if exists(&self.directory.join(name))? {
        history.complete = false;
        history.warning = Some(
          "Legacy JSONL audit files remain in the history directory; they are not imported into SQLite.",
        );
      }
    }
    if !exists(&self.directory.join("audit.sqlite3"))? {
      return Ok(history);
    }
    let connection = self.database(false)?;
    check_schema(&connection)?;
    let mut statement = connection
      .prepare(
        "SELECT substr(event_id, 1, 37), substr(run_id, 1, 37), timestamp_ms,
         substr(subject_id, 1, 65), substr(outcome, 1, 81),
         CASE WHEN length(CAST(record AS BLOB)) <= 2048 THEN record ELSE NULL END FROM events
       WHERE (?1 IS NULL OR run_id = ?1)
         AND (?2 = 0 OR outcome IN ('failed', 'interrupted'))
       ORDER BY sequence DESC",
      )
      .map_err(|error| sql_error(&error))?;
    let mut rows = statement
      .query(params![run.map(|id| id.to_string()), failed_only])
      .map_err(|error| sql_error(&error))?;
    while let Some(row) = rows.next().map_err(|error| sql_error(&error))? {
      let record = (|| -> rusqlite::Result<Option<Record>> {
        let text: String = row.get(5)?;
        if text.len() > MAX_RECORD_BYTES {
          return Ok(None);
        }
        let Some(record) = serde_json::from_str::<Record>(&text)
          .ok()
          .filter(Record::valid)
        else {
          return Ok(None);
        };
        // Indexed metadata must agree with the validated, allowlisted payload.
        let valid = row.get::<_, String>(0)? == record.event_id.to_string()
          && row.get::<_, String>(1)? == record.run_id.to_string()
          && u64::try_from(row.get::<_, i64>(2)?).ok() == Some(record.timestamp_ms)
          && row.get::<_, Option<String>>(3)? == record.subject_id
          && row.get::<_, String>(4)? == enum_text(record.outcome).unwrap_or_default();
        Ok(valid.then_some(record))
      })();
      if let Ok(Some(record)) = record {
        if level.is_some_and(|level| record.level < level) {
          continue;
        }
        history.records.push(record);
        if history.records.len() == limit {
          break;
        }
      } else {
        history.complete = false;
        history.warning =
          Some("Some audit records are malformed or use an unsupported schema; they were omitted.");
      }
    }
    history.records.reverse();
    Ok(history)
  }
}

fn enum_text(value: super::Outcome) -> io::Result<String> {
  serde_json::to_value(value)
    .map_err(io::Error::other)?
    .as_str()
    .map(str::to_owned)
    .ok_or_else(|| io::Error::other("invalid audit outcome"))
}

fn check_schema(connection: &Connection) -> io::Result<()> {
  let application: i32 = connection
    .pragma_query_value(None, "application_id", |row| row.get(0))
    .map_err(|error| sql_error(&error))?;
  let version: i32 = connection
    .pragma_query_value(None, "user_version", |row| row.get(0))
    .map_err(|error| sql_error(&error))?;
  if application != APPLICATION_ID || version != SCHEMA_VERSION {
    return Err(io::Error::other(
      "unrecognized or unsupported audit database schema",
    ));
  }
  Ok(())
}

fn sql_error(error: &rusqlite::Error) -> io::Error {
  let kind = match error.sqlite_error_code() {
    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
      io::ErrorKind::WouldBlock
    }
    _ => io::ErrorKind::Other,
  };
  // Do not include database content or SQL parameters in stderr diagnostics.
  io::Error::new(
    kind,
    format!(
      "audit database operation failed ({:?})",
      error.sqlite_error_code()
    ),
  )
}
