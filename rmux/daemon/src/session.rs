use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_core::{JournalError, JournalSnapshot, OutputChunk, OutputJournal, validate_session_name};
use rmux_proto::{CommandSpec, SessionInfo, SessionStatus, TerminalSize};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{Notify, broadcast};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SessionEvent {
  Output(OutputChunk),
  Ended { exit_code: Option<u32> },
}

pub struct Session {
  id: String,
  name: String,
  created_at_ms: u64,
  terminal_size: Mutex<TerminalSize>,
  journal: Mutex<OutputJournal>,
  master: Mutex<Box<dyn MasterPty + Send>>,
  writer: Mutex<Box<dyn Write + Send>>,
  killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
  events: broadcast::Sender<SessionEvent>,
}

impl Session {
  pub fn info(&self) -> SessionInfo {
    SessionInfo {
      session_id: self.id.clone(),
      name: self.name.clone(),
      status: SessionStatus::Running,
      created_at_ms: self.created_at_ms,
      next_sequence: lock(&self.journal).next_sequence(),
      terminal_size: lock(&self.terminal_size).clone(),
    }
  }

  pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
    self.events.subscribe()
  }

  pub fn snapshot_from(&self, requested: Option<u64>) -> Result<JournalSnapshot, JournalError> {
    lock(&self.journal).snapshot_from(requested)
  }

  pub fn write_input(&self, data: &[u8]) -> Result<(), SessionControlError> {
    let mut writer = lock(&self.writer);
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
  }

  pub fn resize(&self, terminal_size: TerminalSize) -> Result<(), SessionControlError> {
    lock(&self.master)
      .resize(to_pty_size(&terminal_size))
      .map_err(|error| SessionControlError::Pty(error.to_string()))?;
    *lock(&self.terminal_size) = terminal_size;
    Ok(())
  }

  pub fn kill(&self) -> Result<(), SessionControlError> {
    lock(&self.killer).kill()?;
    Ok(())
  }

  fn append_output(&self, data: &[u8]) {
    let chunk = lock(&self.journal).append(data);
    if let Some(chunk) = chunk {
      let _ignored = self.events.send(SessionEvent::Output(chunk));
    }
  }

  fn publish_ended(&self, exit_code: Option<u32>) {
    let _ignored = self.events.send(SessionEvent::Ended { exit_code });
  }
}

#[derive(Clone)]
pub struct SessionManager {
  inner: Arc<SessionManagerInner>,
}

struct SessionManagerInner {
  registry: Mutex<SessionRegistry>,
  journal_capacity_bytes: usize,
  ever_had_session: AtomicBool,
  changed: Notify,
}

#[derive(Default)]
struct SessionRegistry {
  sessions: HashMap<String, Arc<Session>>,
  pending_names: HashSet<String>,
}

impl SessionManager {
  pub fn new(journal_capacity_bytes: usize) -> Self {
    Self {
      inner: Arc::new(SessionManagerInner {
        registry: Mutex::new(SessionRegistry::default()),
        journal_capacity_bytes,
        ever_had_session: AtomicBool::new(false),
        changed: Notify::new(),
      }),
    }
  }

  pub fn create(
    &self,
    requested_name: Option<String>,
    command: Option<CommandSpec>,
    working_directory: Option<String>,
    terminal_size: TerminalSize,
  ) -> Result<Arc<Session>, SessionManagerError> {
    let session_id = Uuid::new_v4().to_string();
    let name = requested_name.unwrap_or_else(|| format!("session-{}", &session_id[..8]));
    validate_session_name(&name).map_err(|message| SessionManagerError::InvalidName {
      message: message.into(),
    })?;

    let reservation = NameReservation::acquire(Arc::clone(&self.inner), name.clone())?;

    let pty_system = native_pty_system();
    let pair = pty_system
      .openpty(to_pty_size(&terminal_size))
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let mut command_builder = build_command(command, working_directory);
    command_builder.env("TERM", "xterm-256color");
    let mut child = pair
      .slave
      .spawn_command(command_builder)
      .map_err(|error| SessionManagerError::Spawn(error.to_string()))?;
    let reader = pair
      .master
      .try_clone_reader()
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let writer = pair
      .master
      .take_writer()
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let killer = child.clone_killer();
    drop(pair.slave);

    let (events, _) = broadcast::channel(256);
    let session = Arc::new(Session {
      id: session_id.clone(),
      name,
      created_at_ms: unix_time_ms(),
      terminal_size: Mutex::new(terminal_size),
      journal: Mutex::new(OutputJournal::new(self.inner.journal_capacity_bytes)),
      master: Mutex::new(pair.master),
      writer: Mutex::new(writer),
      killer: Mutex::new(killer),
      events,
    });

    reservation.commit(session_id.clone(), Arc::clone(&session));
    self.inner.ever_had_session.store(true, Ordering::Release);
    self.inner.changed.notify_one();

    let reader_session = Arc::clone(&session);
    let reader_thread = std::thread::Builder::new()
      .name(format!("rmux-reader-{}", &session_id[..8]))
      .spawn(move || read_pty(reader, &reader_session))
      .map_err(SessionManagerError::ReaderThread)?;

    let manager = Arc::downgrade(&self.inner);
    let waiter_session = Arc::clone(&session);
    std::thread::Builder::new()
      .name(format!("rmux-waiter-{}", &session_id[..8]))
      .spawn(move || {
        let exit_code = child.wait().ok().map(|status| status.exit_code());
        let _reader_result = reader_thread.join();
        waiter_session.publish_ended(exit_code);
        if let Some(manager) = manager.upgrade() {
          lock(&manager.registry).sessions.remove(&session_id);
          manager.changed.notify_one();
        }
      })
      .map_err(SessionManagerError::WaiterThread)?;

    Ok(session)
  }

  pub fn list(&self) -> Vec<SessionInfo> {
    let mut sessions: Vec<_> = lock(&self.inner.registry)
      .sessions
      .values()
      .map(|session| session.info())
      .collect();
    sessions.sort_by(|left, right| {
      left
        .created_at_ms
        .cmp(&right.created_at_ms)
        .then_with(|| left.name.cmp(&right.name))
    });
    sessions
  }

  pub fn resolve(&self, selector: &str) -> Result<Arc<Session>, SessionManagerError> {
    let registry = lock(&self.inner.registry);
    if let Some(session) = registry.sessions.get(selector) {
      return Ok(Arc::clone(session));
    }

    registry
      .sessions
      .values()
      .find(|session| session.name == selector)
      .cloned()
      .ok_or_else(|| SessionManagerError::NotFound {
        selector: selector.into(),
      })
  }

  pub fn session_count(&self) -> usize {
    lock(&self.inner.registry).sessions.len()
  }

  pub fn ever_had_session(&self) -> bool {
    self.inner.ever_had_session.load(Ordering::Acquire)
  }

  pub async fn changed(&self) {
    self.inner.changed.notified().await;
  }
}

struct NameReservation {
  manager: Arc<SessionManagerInner>,
  name: String,
  active: bool,
}

impl NameReservation {
  fn acquire(manager: Arc<SessionManagerInner>, name: String) -> Result<Self, SessionManagerError> {
    {
      let mut registry = lock(&manager.registry);
      let exists = registry.pending_names.contains(&name)
        || registry
          .sessions
          .values()
          .any(|session| session.name == name);
      if exists {
        return Err(SessionManagerError::AlreadyExists { name });
      }
      registry.pending_names.insert(name.clone());
    }

    Ok(Self {
      manager,
      name,
      active: true,
    })
  }

  fn commit(mut self, session_id: String, session: Arc<Session>) {
    let mut registry = lock(&self.manager.registry);
    registry.pending_names.remove(&self.name);
    registry.sessions.insert(session_id, session);
    self.active = false;
  }
}

impl Drop for NameReservation {
  fn drop(&mut self) {
    if self.active {
      lock(&self.manager.registry)
        .pending_names
        .remove(&self.name);
    }
  }
}

#[derive(Debug, Error)]
pub enum SessionManagerError {
  #[error("invalid session name: {message}")]
  InvalidName { message: String },
  #[error("a session named '{name}' already exists")]
  AlreadyExists { name: String },
  #[error("session '{selector}' was not found")]
  NotFound { selector: String },
  #[error("could not create PTY: {0}")]
  Pty(String),
  #[error("could not spawn child process: {0}")]
  Spawn(String),
  #[error("could not start PTY reader thread: {0}")]
  ReaderThread(std::io::Error),
  #[error("could not start child waiter thread: {0}")]
  WaiterThread(std::io::Error),
}

#[derive(Debug, Error)]
pub enum SessionControlError {
  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),
  #[error("PTY error: {0}")]
  Pty(String),
}

fn build_command(
  command: Option<CommandSpec>,
  working_directory: Option<String>,
) -> CommandBuilder {
  let mut builder = if let Some(command) = command {
    let mut builder = CommandBuilder::new(command.program);
    builder.args(command.arguments);
    builder
  } else {
    CommandBuilder::new_default_prog()
  };

  if let Some(working_directory) = working_directory {
    builder.cwd(working_directory);
  }
  builder
}

fn read_pty(mut reader: Box<dyn Read + Send>, session: &Session) {
  let mut buffer = vec![0_u8; 16 * 1024];
  loop {
    match reader.read(&mut buffer) {
      Ok(0) | Err(_) => return,
      Ok(bytes_read) => session.append_output(&buffer[..bytes_read]),
    }
  }
}

fn to_pty_size(size: &TerminalSize) -> PtySize {
  PtySize {
    rows: size.rows,
    cols: size.columns,
    pixel_width: size.pixel_width,
    pixel_height: size.pixel_height,
  }
}

fn unix_time_ms() -> u64 {
  let millis = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis();
  u64::try_from(millis).unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}
