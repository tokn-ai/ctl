//! Local diagnostic and audit history. Records contain allowlisted metadata only.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io;
use std::sync::{
  OnceLock,
  atomic::{AtomicBool, Ordering},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

mod storage;
pub use storage::{History, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
  Logs,
  Audit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
  DaemonLifecycle,
  HelperRequest,
  ProxyConnection,
  Connection,
  Disconnect,
  CredentialRead,
  CredentialSave,
  CredentialRemove,
  CredentialClear,
  CredentialInventory,
  IdentitySave,
  IdentityRemove,
  IdentityInventory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
  Started,
  Succeeded,
  Missing,
  Failed,
  Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
  pub schema_version: u32,
  pub event_id: Uuid,
  pub operation_id: Uuid,
  pub timestamp_ms: u64,
  pub process_id: u32,
  pub event: Event,
  pub outcome: Outcome,
  pub subject_id: Option<String>,
  pub elapsed_ms: u64,
  pub error_code: Option<String>,
  pub os_error: Option<i32>,
}

static STORE: OnceLock<Option<Store>> = OnceLock::new();
static WARNED: AtomicBool = AtomicBool::new(false);

/// Enable recording in a ctld entry point. Library tests do not initialize it.
pub fn initialize() {
  STORE.get_or_init(|| match user_store() {
    Ok(store) => Some(store),
    Err(error) => {
      warn(&error);
      None
    }
  });
}

/// Resolve the shared store without creating files.
///
/// # Errors
/// Returns an error when the user's home directory is unavailable.
pub fn user_store() -> io::Result<Store> {
  let base = crate::paths::directory()?;
  Ok(Store::new(base.join("history")))
}

fn warn(error: &io::Error) {
  if !WARNED.swap(true, Ordering::Relaxed) {
    eprintln!("Warning: ctl logging/audit recording failed; the operation will continue: {error}");
  }
}

fn emit(stream: Stream, record: &Record) {
  if let Some(Some(store)) = STORE.get() {
    if let Err(error) = store.append(stream, record) {
      warn(&error);
    } else {
      WARNED.store(false, Ordering::Relaxed);
    }
  }
}

/// An operation's start and terminal outcome share a correlation ID.
/// Dropping an unfinished operation records interruption, never success.
pub struct Operation {
  record: Record,
  started: Instant,
  finished: bool,
  audit: bool,
}

impl Operation {
  #[must_use]
  pub fn start(event: Event, subject: Option<&str>) -> Self {
    Self::begin(event, subject, true)
  }

  #[must_use]
  pub fn diagnostic(event: Event) -> Self {
    Self::begin(event, None, false)
  }

  fn begin(event: Event, subject: Option<&str>, audit: bool) -> Self {
    let record = Record {
      schema_version: 1,
      event_id: Uuid::new_v4(),
      operation_id: Uuid::new_v4(),
      timestamp_ms: now(),
      process_id: std::process::id(),
      event,
      outcome: Outcome::Started,
      subject_id: subject.map(|value| format!("{:x}", Sha256::digest(value.as_bytes()))),
      elapsed_ms: 0,
      error_code: None,
      os_error: None,
    };
    emit(Stream::Logs, &record);
    if audit {
      emit(Stream::Audit, &record);
    }
    Self {
      record,
      started: Instant::now(),
      finished: false,
      audit,
    }
  }

  /// Error codes must be fixed, non-secret classifications, never error messages.
  pub fn finish(
    mut self,
    outcome: Outcome,
    error_code: Option<&'static str>,
    os_error: Option<i32>,
  ) {
    self.complete(outcome, error_code, os_error);
  }

  fn complete(
    &mut self,
    outcome: Outcome,
    error_code: Option<&'static str>,
    os_error: Option<i32>,
  ) {
    self.finished = true;
    self.record.event_id = Uuid::new_v4();
    self.record.timestamp_ms = now();
    self.record.elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    self.record.outcome = outcome;
    self.record.error_code = error_code.map(str::to_owned);
    self.record.os_error = os_error;
    emit(Stream::Logs, &self.record);
    if self.audit {
      emit(Stream::Audit, &self.record);
    }
  }
}

impl Drop for Operation {
  fn drop(&mut self) {
    if !self.finished {
      self.complete(Outcome::Interrupted, None, None);
    }
  }
}

fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|value| u64::try_from(value.as_millis()).ok())
    .unwrap_or(0)
}

impl Record {
  fn valid(&self) -> bool {
    self.schema_version == 1
      && self
        .subject_id
        .as_ref()
        .is_none_or(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
      && self.error_code.as_ref().is_none_or(|value| {
        value.len() <= 80
          && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
      })
  }
}

#[cfg(test)]
mod tests;
