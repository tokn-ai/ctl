use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_core::{JournalError, JournalSnapshot, OutputChunk, OutputJournal, validate_session_name};
use rmux_proto::{
  CommandSpec, SessionInfo, SessionStatus, TERMINAL_CHECKPOINT_FORMAT,
  TERMINAL_CHECKPOINT_FORMAT_VERSION, TerminalCheckpoint, TerminalSize,
};
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
  terminal: Mutex<TerminalState>,
  checkpoint_interval_bytes: u64,
  master: Mutex<Box<dyn MasterPty + Send>>,
  writer: Mutex<Box<dyn Write + Send>>,
  killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
  events: broadcast::Sender<SessionEvent>,
}

struct TerminalState {
  terminal: avt::Vt,
  pending_input: Vec<u8>,
  journal: OutputJournal,
  checkpoint: TerminalCheckpoint,
  terminal_size: TerminalSize,
}

#[derive(Debug, Clone)]
pub struct AttachSnapshot {
  pub checkpoint: Option<TerminalCheckpoint>,
  pub journal: JournalSnapshot,
  pub history_gap: bool,
}

impl Session {
  pub fn info(&self) -> SessionInfo {
    let terminal = lock(&self.terminal);
    SessionInfo {
      session_id: self.id.clone(),
      name: self.name.clone(),
      status: SessionStatus::Running,
      created_at_ms: self.created_at_ms,
      next_sequence: terminal.journal.next_sequence(),
      terminal_size: terminal.terminal_size.clone(),
    }
  }

  pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
    self.events.subscribe()
  }

  pub fn snapshot_for_attach(
    &self,
    requested: Option<u64>,
  ) -> Result<AttachSnapshot, JournalError> {
    let terminal = lock(&self.terminal);
    let earliest_sequence = terminal.journal.earliest_sequence();
    if let Some(sequence) = requested
      && sequence > terminal.journal.next_sequence()
    {
      return Err(JournalError::SequenceAhead {
        requested: sequence,
        next: terminal.journal.next_sequence(),
      });
    }

    let needs_checkpoint = requested.is_none_or(|sequence| sequence < earliest_sequence);
    let replay_from = if needs_checkpoint {
      terminal.checkpoint.sequence
    } else {
      requested.unwrap_or(earliest_sequence)
    };
    let journal = terminal.journal.snapshot_from(Some(replay_from))?;

    Ok(AttachSnapshot {
      checkpoint: needs_checkpoint.then(|| terminal.checkpoint.clone()),
      journal,
      history_gap: requested.is_some_and(|sequence| sequence < earliest_sequence),
    })
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
    let mut terminal = lock(&self.terminal);
    terminal.terminal.resize(
      usize::from(terminal_size.columns),
      usize::from(terminal_size.rows),
    );
    terminal.terminal_size = terminal_size;
    refresh_checkpoint(&mut terminal);
    Ok(())
  }

  pub fn kill(&self) -> Result<(), SessionControlError> {
    lock(&self.killer).kill()?;
    Ok(())
  }

  fn append_output(&self, data: &[u8]) {
    let mut terminal = lock(&self.terminal);
    feed_terminal_bytes(&mut terminal, data);
    let chunk = terminal.journal.append(data);
    let checkpoint_is_stale = terminal.journal.earliest_sequence() > terminal.checkpoint.sequence;
    let checkpoint_is_due = terminal
      .journal
      .next_sequence()
      .saturating_sub(terminal.checkpoint.sequence)
      >= self.checkpoint_interval_bytes;
    if checkpoint_is_stale || checkpoint_is_due {
      refresh_checkpoint(&mut terminal);
    }
    drop(terminal);
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
  checkpoint_interval_bytes: usize,
  ever_had_session: AtomicBool,
  changed: Notify,
}

#[derive(Default)]
struct SessionRegistry {
  sessions: HashMap<String, Arc<Session>>,
  pending_names: HashSet<String>,
}

impl SessionManager {
  pub fn new(journal_capacity_bytes: usize, checkpoint_interval_bytes: usize) -> Self {
    Self {
      inner: Arc::new(SessionManagerInner {
        registry: Mutex::new(SessionRegistry::default()),
        journal_capacity_bytes,
        checkpoint_interval_bytes: checkpoint_interval_bytes.max(1),
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
    let terminal_parser = avt::Vt::new(
      usize::from(terminal_size.columns),
      usize::from(terminal_size.rows),
    );
    let mut terminal = TerminalState {
      terminal: terminal_parser,
      pending_input: Vec::new(),
      journal: OutputJournal::new(self.inner.journal_capacity_bytes),
      checkpoint: TerminalCheckpoint {
        format: TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence: 0,
        terminal_size: terminal_size.clone(),
        payload: Vec::new(),
        input_prefix: Vec::new(),
      },
      terminal_size,
    };
    refresh_checkpoint(&mut terminal);
    let session = Arc::new(Session {
      id: session_id.clone(),
      name,
      created_at_ms: unix_time_ms(),
      terminal: Mutex::new(terminal),
      checkpoint_interval_bytes: self.inner.checkpoint_interval_bytes as u64,
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

fn refresh_checkpoint(terminal: &mut TerminalState) {
  terminal.checkpoint = TerminalCheckpoint {
    format: TERMINAL_CHECKPOINT_FORMAT.into(),
    format_version: TERMINAL_CHECKPOINT_FORMAT_VERSION,
    sequence: terminal.journal.next_sequence(),
    terminal_size: terminal.terminal_size.clone(),
    payload: terminal.terminal.dump().into_bytes(),
    input_prefix: terminal.pending_input.clone(),
  };
}

fn feed_terminal_bytes(terminal: &mut TerminalState, data: &[u8]) {
  terminal.pending_input.extend_from_slice(data);

  loop {
    match std::str::from_utf8(&terminal.pending_input) {
      Ok(valid) => {
        terminal.terminal.feed_str(valid);
        terminal.pending_input.clear();
        return;
      }
      Err(error) => {
        let valid_up_to = error.valid_up_to();
        if valid_up_to > 0 {
          let valid = String::from_utf8(terminal.pending_input[..valid_up_to].to_vec())
            .expect("valid UTF-8 prefix reported by std::str::from_utf8");
          terminal.terminal.feed_str(&valid);
          terminal.pending_input.drain(..valid_up_to);
          continue;
        }

        let Some(invalid_length) = error.error_len() else {
          return;
        };
        terminal.terminal.feed('\u{fffd}');
        terminal.pending_input.drain(..invalid_length);
      }
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn checkpoint_preserves_a_partial_escape_sequence() {
    let mut source = terminal_state();
    feed_terminal_bytes(&mut source, b"\x1b[");
    refresh_checkpoint(&mut source);
    let checkpoint = source.checkpoint.clone();

    feed_terminal_bytes(&mut source, b"2J\x1b[Hcheckpoint");
    let mut restored = terminal_state_from_checkpoint(checkpoint);
    feed_terminal_bytes(&mut restored, b"2J\x1b[Hcheckpoint");

    assert_eq!(source.terminal.text(), restored.terminal.text());
    assert!(restored.terminal.text().join("\n").contains("checkpoint"));
  }

  #[test]
  fn checkpoint_preserves_an_incomplete_utf8_character() {
    let mut source = terminal_state();
    feed_terminal_bytes(&mut source, &[0xe6]);
    refresh_checkpoint(&mut source);
    let checkpoint = source.checkpoint.clone();

    feed_terminal_bytes(&mut source, &[0x97, 0xa5]);
    let mut restored = terminal_state_from_checkpoint(checkpoint);
    feed_terminal_bytes(&mut restored, &[0x97, 0xa5]);

    assert_eq!(source.terminal.text(), restored.terminal.text());
    assert!(restored.terminal.text().join("\n").contains('日'));
  }

  fn terminal_state() -> TerminalState {
    let terminal_size = TerminalSize::default();
    let mut state = TerminalState {
      terminal: avt::Vt::new(80, 24),
      pending_input: Vec::new(),
      journal: OutputJournal::new(1024),
      checkpoint: TerminalCheckpoint {
        format: TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence: 0,
        terminal_size: terminal_size.clone(),
        payload: Vec::new(),
        input_prefix: Vec::new(),
      },
      terminal_size,
    };
    refresh_checkpoint(&mut state);
    state
  }

  fn terminal_state_from_checkpoint(checkpoint: TerminalCheckpoint) -> TerminalState {
    let mut terminal = avt::Vt::new(
      usize::from(checkpoint.terminal_size.columns),
      usize::from(checkpoint.terminal_size.rows),
    );
    let payload = String::from_utf8(checkpoint.payload.clone())
      .expect("checkpoint payload is generated from a UTF-8 VT dump");
    terminal.feed_str(&payload);
    TerminalState {
      terminal,
      pending_input: checkpoint.input_prefix.clone(),
      journal: OutputJournal::new(1024),
      terminal_size: checkpoint.terminal_size.clone(),
      checkpoint,
    }
  }
}
