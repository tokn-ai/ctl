//! Local diagnostic and audit history. Records contain allowlisted metadata only.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::{self, IsTerminal as _, Write as _};
use std::sync::{
  OnceLock,
  atomic::{AtomicBool, Ordering},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

mod message;
mod storage;
mod text;
pub use storage::{History, Store};

/// Minimum diagnostic severity. Audit recording is independent of this filter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
  Trace,
  Debug,
  #[default]
  Info,
  Warn,
  Error,
}

impl std::str::FromStr for Level {
  type Err = &'static str;
  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value.to_ascii_lowercase().as_str() {
      "trace" => Ok(Self::Trace),
      "debug" => Ok(Self::Debug),
      "info" => Ok(Self::Info),
      "warn" => Ok(Self::Warn),
      "error" => Ok(Self::Error),
      _ => Err("log level must be trace, debug, info, warn, or error"),
    }
  }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
  #[default]
  Ctld,
  Ctmuxd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lease {
  Input,
  Layout,
}

/// Only generated UUIDs are allowed in diagnostic context, never names or tokens.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
  pub session_id: Option<Uuid>,
  pub pane_id: Option<Uuid>,
  pub attachment_id: Option<Uuid>,
  pub exit_code: Option<u32>,
  pub lease: Option<Lease>,
}

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
  ConnectionRequest,
  ConnectionReuse,
  Disconnect,
  CredentialRead,
  CredentialSave,
  CredentialRemove,
  CredentialClear,
  CredentialInventory,
  IdentitySave,
  IdentityRemove,
  IdentityInventory,
  SessionCreate,
  SessionTerminate,
  SessionMerge,
  PaneSplit,
  PanePromote,
  PaneKill,
  PaneExit,
  PaneResize,
  ViewResize,
  DividerResize,
  PaneZoom,
  ViewUpdate,
  AttachmentCreate,
  AttachmentResume,
  AttachmentSuspend,
  AttachmentExpire,
  AttachmentDetach,
  LeaseAcquire,
  LeaseRelease,
  SessionTransport,
  ControlTransport,
  LogConfiguration,
  VpnMonitor,
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

/// Submitted SSH endpoint only; excludes keys, route configuration and credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionEndpoint {
  pub destination: String,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
}

impl ConnectionEndpoint {
  #[must_use]
  pub fn display(&self) -> String {
    let host = self.hostname.as_deref().unwrap_or(&self.destination);
    let host = if host.contains(':') {
      format!("[{host}]")
    } else {
      host.to_owned()
    };
    let user = self
      .user
      .as_ref()
      .map_or_else(String::new, |user| format!("{user}@"));
    let port = self
      .port
      .map_or_else(String::new, |port| format!(":{port}"));
    format!("{user}{host}{port}")
  }

  fn valid(&self) -> bool {
    let valid_text = |value: &str| {
      !value.is_empty()
        && value.len() <= 1024
        && !value
          .chars()
          .any(|value| value.is_control() || value.is_whitespace())
    };
    valid_text(&self.destination)
      && self.hostname.as_deref().is_none_or(valid_text)
      && self.user.as_deref().is_none_or(valid_text)
      && self.port != Some(0)
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
  pub schema_version: u32,
  #[serde(default)]
  pub level: Level,
  #[serde(default)]
  pub component: Component,
  #[serde(default)]
  pub context: Context,
  pub run_id: Uuid,
  pub event_id: Uuid,
  pub operation_id: Uuid,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub attempt_id: Option<Uuid>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub correlation_id: Option<Uuid>,
  pub timestamp_ms: u64,
  pub process_id: u32,
  pub event: Event,
  pub outcome: Outcome,
  pub subject_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub connection_endpoint: Option<ConnectionEndpoint>,
  pub elapsed_ms: u64,
  pub error_code: Option<String>,
  pub os_error: Option<i32>,
}

static COMPONENT: OnceLock<Component> = OnceLock::new();
static LOG_LEVEL: OnceLock<Level> = OnceLock::new();
static CONSOLE: OnceLock<bool> = OnceLock::new();

static RUN_ID: OnceLock<Uuid> = OnceLock::new();

fn run_id() -> Uuid {
  *RUN_ID.get_or_init(Uuid::new_v4)
}

static STORE: OnceLock<Option<Store>> = OnceLock::new();
static WARNED: AtomicBool = AtomicBool::new(false);

/// Enable recording in a ctld entry point. Library tests do not initialize it.
pub fn initialize() {
  initialize_component(Component::Ctld);
}

/// Enable one daemon's recorder, choosing its component and minimum log level.
pub fn initialize_component(component: Component) {
  initialize_recorder(component, io::stderr().is_terminal());
}

/// Foreground daemons write to stderr even when it is redirected. Auto-started,
/// detached daemons use files; helper protocol entry points use `initialize`.
pub fn initialize_daemon(component: Component, detached: bool) {
  initialize_recorder(component, !detached);
}

fn initialize_recorder(component: Component, console: bool) {
  CONSOLE.get_or_init(|| console);
  COMPONENT.get_or_init(|| component);
  let mut invalid_level = false;
  LOG_LEVEL.get_or_init(|| match std::env::var("CTL_LOG_LEVEL") {
    Ok(value) => value.parse().unwrap_or_else(|_| {
      invalid_level = true;
      Level::Info
    }),
    Err(std::env::VarError::NotPresent) => Level::Info,
    Err(std::env::VarError::NotUnicode(_)) => {
      invalid_level = true;
      Level::Info
    }
  });
  STORE.get_or_init(|| match user_store() {
    Ok(store) => Some(store),
    Err(error) => {
      warn(&error);
      None
    }
  });
  if invalid_level {
    diagnostic_event(
      "19f3f37a-5c45-4dac-bb36-d25987124d3a",
      Event::LogConfiguration,
      Level::Warn,
      Context::default(),
      Outcome::Failed,
      None,
      None,
    );
  }
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
    let _ = writeln!(
      io::stderr().lock(),
      "Warning: ctl logging/audit recording failed; the operation will continue: {error}"
    );
  }
}

fn emit(stream: Stream, record: &Record) {
  if STORE.get().is_none() {
    return;
  }
  if stream == Stream::Logs && record.level < *LOG_LEVEL.get().unwrap_or(&Level::Info) {
    return;
  }
  // Foreground diagnostics share the file renderer; detached daemons still save
  // records even when their launcher discards stderr. Never mirror audit twice.
  if stream == Stream::Logs
    && *CONSOLE.get().unwrap_or(&false)
    && let Ok(line) = record.log_line()
  {
    // A closed console pipe must not turn a diagnostic into a daemon failure.
    let _ = writeln!(io::stderr().lock(), "{line}");
  }
  if let Some(Some(store)) = STORE.get() {
    if let Err(error) = store.append(stream, record) {
      warn(&error);
    } else {
      WARNED.store(false, Ordering::Relaxed);
    }
  }
}

/// Record an instantaneous diagnostic once, without an artificial start pair.
/// Error codes must be fixed classifications, never raw error text.
pub fn diagnostic_event(
  operation_id: &'static str,
  event: Event,
  level: Level,
  context: Context,
  outcome: Outcome,
  error_code: Option<&'static str>,
  os_error: Option<i32>,
) {
  let mut record = new_record(operation_id, event, level, context, None);
  record.outcome = outcome;
  record.attempt_id = None;
  record.error_code = error_code.map(str::to_owned);
  record.os_error = os_error;
  emit(Stream::Logs, &record);
}

/// Reuse is an observation, not another authentication attempt or audit operation.
pub fn connection_reused(
  operation_id: &'static str,
  subject: &str,
  endpoint: ConnectionEndpoint,
  correlation_id: Uuid,
) {
  let mut record = new_record(
    operation_id,
    Event::ConnectionReuse,
    Level::Debug,
    Context::default(),
    Some(subject),
  );
  record.connection_endpoint = Some(endpoint);
  record.correlation_id = Some(correlation_id);
  record.attempt_id = None;
  record.outcome = Outcome::Succeeded;
  emit(Stream::Logs, &record);
}

fn new_record(
  operation_id: &'static str,
  event: Event,
  level: Level,
  context: Context,
  subject: Option<&str>,
) -> Record {
  Record {
    schema_version: 6,
    level,
    context,
    component: *COMPONENT.get().unwrap_or(&Component::Ctld),
    run_id: run_id(),
    event_id: Uuid::new_v4(),
    operation_id: Uuid::parse_str(operation_id).expect("call-site operation ID is a UUID literal"),
    attempt_id: Some(Uuid::new_v4()),
    correlation_id: None,
    timestamp_ms: now(),
    process_id: std::process::id(),
    event,
    outcome: Outcome::Started,
    connection_endpoint: None,
    subject_id: subject.map(|value| format!("{:x}", Sha256::digest(value.as_bytes()))),
    elapsed_ms: 0,
    error_code: None,
    os_error: None,
  }
}

/// A stable call-site operation ID identifies the source; attempt ID pairs outcomes.
/// Dropping an unfinished operation records interruption, never success.
pub struct Operation {
  record: Record,
  started: Instant,
  finished: bool,
  audit: bool,
}

impl Operation {
  #[must_use]
  pub fn start(operation_id: &'static str, event: Event, subject: Option<&str>) -> Self {
    Self::begin(
      operation_id,
      event,
      subject,
      true,
      Level::Info,
      Context::default(),
    )
  }

  #[must_use]
  pub fn diagnostic(operation_id: &'static str, event: Event) -> Self {
    Self::diagnostic_at(operation_id, event, Level::Info, Context::default())
  }

  #[must_use]
  pub fn diagnostic_at(
    operation_id: &'static str,
    event: Event,
    level: Level,
    context: Context,
  ) -> Self {
    Self::begin(operation_id, event, None, false, level, context)
  }

  /// Add generated identifiers learned during an operation, before its outcome.
  pub fn set_context(&mut self, context: Context) {
    self.record.context = context;
  }

  /// Record a connection operation with the submitted nonsecret endpoint.
  #[must_use]
  pub fn connection(
    operation_id: &'static str,
    event: Event,
    subject: &str,
    endpoint: ConnectionEndpoint,
    correlation_id: Option<Uuid>,
  ) -> Self {
    Self::begin_with_endpoint(
      operation_id,
      event,
      Some(subject),
      true,
      Level::Info,
      Context::default(),
      Some((endpoint, correlation_id)),
    )
  }

  #[must_use]
  pub fn connection_request(
    operation_id: &'static str,
    subject: &str,
    endpoint: ConnectionEndpoint,
    correlation_id: Option<Uuid>,
  ) -> Self {
    Self::begin_with_endpoint(
      operation_id,
      Event::ConnectionRequest,
      Some(subject),
      false,
      Level::Debug,
      Context::default(),
      Some((endpoint, correlation_id)),
    )
  }

  fn begin(
    operation_id: &'static str,
    event: Event,
    subject: Option<&str>,
    audit: bool,
    level: Level,
    context: Context,
  ) -> Self {
    Self::begin_with_endpoint(operation_id, event, subject, audit, level, context, None)
  }

  fn begin_with_endpoint(
    operation_id: &'static str,
    event: Event,
    subject: Option<&str>,
    audit: bool,
    level: Level,
    context: Context,
    endpoint: Option<(ConnectionEndpoint, Option<Uuid>)>,
  ) -> Self {
    let mut record = new_record(operation_id, event, level, context, subject);
    if let Some((endpoint, correlation_id)) = endpoint {
      record.connection_endpoint = Some(endpoint);
      record.correlation_id = correlation_id;
    }
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
    self.complete(outcome, error_code, os_error, None);
  }

  /// Record an explicitly classified severity, including expected rejections.
  pub fn finish_at(
    mut self,
    outcome: Outcome,
    level: Level,
    error_code: Option<&'static str>,
    os_error: Option<i32>,
  ) {
    self.complete(outcome, error_code, os_error, Some(level));
  }

  fn complete(
    &mut self,
    outcome: Outcome,
    error_code: Option<&'static str>,
    os_error: Option<i32>,
    level: Option<Level>,
  ) {
    self.finished = true;
    self.record.event_id = Uuid::new_v4();
    self.record.timestamp_ms = now();
    self.record.elapsed_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    self.record.outcome = outcome;
    self.record.level = level.unwrap_or_else(|| match outcome {
      Outcome::Failed => Level::Error,
      Outcome::Interrupted => self.record.level.max(Level::Warn),
      _ => self.record.level,
    });
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
      self.complete(Outcome::Interrupted, None, None, None);
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
    matches!(self.schema_version, 2..=6)
      && (self.schema_version < 6 || !self.operation_id.is_nil())
      && self
        .connection_endpoint
        .as_ref()
        .is_none_or(|endpoint| self.schema_version >= 5 && endpoint.valid())
      && i64::try_from(self.timestamp_ms).is_ok()
      && i64::try_from(self.elapsed_ms).is_ok()
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
