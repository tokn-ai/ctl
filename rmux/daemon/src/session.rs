use crate::shell_reporter::{ShellReport, ShellReporter, ShellReporterError};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_core::{
  AttachmentLeaseRegistry, AttachmentLeases, JournalError, JournalSnapshot, OutputChunk,
  OutputJournal, validate_session_name,
};
use rmux_proto::{
  CommandSpec, LeaseKind, LeaseStatus, PromptPhase, SessionInfo, SessionStatus, ShellState,
  TERMINAL_CHECKPOINT_FORMAT, TERMINAL_CHECKPOINT_FORMAT_VERSION, TerminalCheckpoint, TerminalSize,
  TuiHint,
};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{Notify, broadcast, watch};
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
  leases: Mutex<AttachmentLeaseRegistry>,
  master: Mutex<Box<dyn MasterPty + Send>>,
  writer: Mutex<Box<dyn Write + Send>>,
  killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
  events: broadcast::Sender<SessionEvent>,
  shell_state_publisher: ShellStatePublisher,
  shell_reporter: Mutex<Option<ShellReporter>>,
}

struct TerminalState {
  terminal: avt::Vt,
  pending_input: Vec<u8>,
  journal: OutputJournal,
  checkpoint: TerminalCheckpoint,
  terminal_size: TerminalSize,
  shell_state: ShellState,
  alternate_screen: AlternateScreenTracker,
}

#[derive(Debug, Clone)]
pub struct AttachSnapshot {
  pub checkpoint: Option<TerminalCheckpoint>,
  pub journal: JournalSnapshot,
  pub history_gap: bool,
  /// The internal, unredacted state observed atomically with the journal.
  /// Callers must apply their own attachment visibility policy before sending
  /// it to a client.
  pub shell_state: ShellState,
}

/// Couples shell-state revision assignment with watch publication.
///
/// Every caller holds a [`ShellStatePublication`] from before it mutates the
/// terminal state until after it has published the resulting snapshot. This
/// prevents a delayed older send from overwriting a newer revision in the
/// coalescing watch channel, and lets raw output be broadcast before the state
/// that observes it.
struct ShellStatePublisher {
  gate: Mutex<()>,
  sender: watch::Sender<ShellState>,
}

struct ShellStatePublication<'a> {
  sender: &'a watch::Sender<ShellState>,
  _gate: MutexGuard<'a, ()>,
}

impl ShellStatePublisher {
  fn new(initial_state: ShellState) -> Self {
    let (sender, _) = watch::channel(initial_state);
    Self {
      gate: Mutex::new(()),
      sender,
    }
  }

  fn subscribe(&self) -> watch::Receiver<ShellState> {
    self.sender.subscribe()
  }

  fn begin(&self) -> ShellStatePublication<'_> {
    ShellStatePublication {
      sender: &self.sender,
      _gate: lock(&self.gate),
    }
  }
}

impl ShellStatePublication<'_> {
  fn publish(&self, state: ShellState) {
    self.sender.send_replace(state);
  }
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

  /// Subscribes to the latest complete shell-awareness snapshot.
  ///
  /// A watch channel intentionally coalesces rapid editable-line updates so
  /// they cannot evict raw PTY output from the bounded attachment broadcast.
  pub fn subscribe_shell_state(&self) -> watch::Receiver<ShellState> {
    self.shell_state_publisher.subscribe()
  }

  /// Returns the latest state without a live command buffer.
  ///
  /// This is used for non-attachment inspection. A one-shot query has no
  /// input-lease identity to prove that it may observe sensitive edit text.
  pub fn shell_state_for_inspection(&self) -> ShellState {
    self.shell_state_for_visibility(false)
  }

  /// Returns the latest state filtered for one attachment.
  ///
  /// Command text is visible only when the attachment both opted in and owns
  /// the input lease at the time this snapshot is produced. A client that has
  /// already received text cannot be made to forget it when it later releases
  /// the lease; this policy governs future snapshots only.
  pub fn shell_state_for_attachment(
    &self,
    attachment_id: &str,
    request_command_line: bool,
  ) -> ShellState {
    let may_view_command_line = request_command_line && self.owns_input_lease(attachment_id);
    self.shell_state_for_visibility(may_view_command_line)
  }

  pub fn owns_input_lease(&self, attachment_id: &str) -> bool {
    lock(&self.leases)
      .status(attachment_id, LeaseKind::Input)
      .owned_by_client
  }

  /// Registers an attached connection and grants any unheld requested leases.
  ///
  /// Leases never transfer implicitly. They are released when the connection
  /// that owns them detaches or disconnects.
  pub fn attach(
    &self,
    attachment_id: &str,
    request_input_lease: bool,
    request_layout_lease: bool,
  ) -> AttachmentLeases {
    lock(&self.leases).request_initial(attachment_id, request_input_lease, request_layout_lease)
  }

  pub fn release_attachment(&self, attachment_id: &str) {
    lock(&self.leases).release_attachment(attachment_id);
  }

  pub fn acquire_lease(&self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    lock(&self.leases).acquire(attachment_id, lease)
  }

  pub fn release_lease(&self, attachment_id: &str, lease: LeaseKind) -> LeaseStatus {
    lock(&self.leases).release(attachment_id, lease)
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
      shell_state: terminal.shell_state.clone(),
    })
  }

  pub fn write_input(&self, attachment_id: &str, data: &[u8]) -> Result<(), SessionControlError> {
    let leases = lock(&self.leases);
    if !leases
      .status(attachment_id, LeaseKind::Input)
      .owned_by_client
    {
      return Err(SessionControlError::InputLeaseRequired);
    }
    let mut writer = lock(&self.writer);
    writer.write_all(data)?;
    writer.flush()?;
    drop(writer);
    drop(leases);
    Ok(())
  }

  pub fn resize(
    &self,
    attachment_id: &str,
    terminal_size: TerminalSize,
  ) -> Result<(), SessionControlError> {
    let leases = lock(&self.leases);
    if !leases
      .status(attachment_id, LeaseKind::Layout)
      .owned_by_client
    {
      return Err(SessionControlError::LayoutLeaseRequired);
    }
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
    drop(terminal);
    drop(leases);
    Ok(())
  }

  pub fn kill(&self) -> Result<(), SessionControlError> {
    lock(&self.killer).kill()?;
    Ok(())
  }

  fn append_output(&self, data: &[u8]) {
    let publication = self.shell_state_publisher.begin();
    let (chunk, shell_state) = {
      let mut terminal = lock(&self.terminal);
      let alternate_screen = terminal.alternate_screen.observe(data);
      feed_terminal_bytes(&mut terminal, data);
      let chunk = terminal.journal.append(data);
      let shell_state = alternate_screen.and_then(|tui_hint| {
        (terminal.shell_state.tui_hint != tui_hint).then(|| {
          terminal.shell_state.tui_hint = tui_hint;
          revise_shell_state(&mut terminal)
        })
      });
      let checkpoint_is_stale = terminal.journal.earliest_sequence() > terminal.checkpoint.sequence;
      let checkpoint_is_due = terminal
        .journal
        .next_sequence()
        .saturating_sub(terminal.checkpoint.sequence)
        >= self.checkpoint_interval_bytes;
      if checkpoint_is_stale || checkpoint_is_due {
        refresh_checkpoint(&mut terminal);
      }
      (chunk, shell_state)
    };
    if let Some(chunk) = chunk {
      let _ignored = self.events.send(SessionEvent::Output(chunk));
    }
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
  }

  fn publish_ended(&self, exit_code: Option<u32>) {
    self.shutdown_shell_reporter();
    let publication = self.shell_state_publisher.begin();
    let shell_state = {
      let mut terminal = lock(&self.terminal);
      if terminal.shell_state.current_command_line.is_none()
        && terminal.shell_state.prompt_phase == PromptPhase::Unknown
      {
        None
      } else {
        terminal.shell_state.current_command_line = None;
        terminal.shell_state.command_line_redacted = false;
        terminal.shell_state.prompt_phase = PromptPhase::Unknown;
        Some(revise_shell_state(&mut terminal))
      }
    };
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
    let _ignored = self.events.send(SessionEvent::Ended { exit_code });
  }

  fn apply_shell_report(&self, report: ShellReport) {
    let publication = self.shell_state_publisher.begin();
    let shell_state = {
      let mut terminal = lock(&self.terminal);
      apply_shell_report_to_terminal(&mut terminal, report)
    };
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
  }

  /// Emits a newer complete state snapshot after a connection transitions into
  /// command-line visibility. The underlying shell report is unchanged, but
  /// the revision keeps every attachment's state stream strictly monotonic.
  pub fn refresh_shell_state_for_visibility(&self) {
    let publication = self.shell_state_publisher.begin();
    let shell_state = {
      let mut terminal = lock(&self.terminal);
      if terminal.shell_state.current_command_line.is_none() {
        None
      } else {
        Some(revise_shell_state(&mut terminal))
      }
    };
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
  }

  fn shutdown_shell_reporter(&self) {
    let reporter = lock(&self.shell_reporter).take();
    if let Some(mut reporter) = reporter {
      let _ignored = reporter.shutdown();
    }
  }

  fn shell_state_for_visibility(&self, may_view_command_line: bool) -> ShellState {
    let mut shell_state = lock(&self.terminal).shell_state.clone();
    shell_state.command_line_redacted = false;
    if !may_view_command_line && shell_state.current_command_line.is_some() {
      shell_state.current_command_line = None;
      shell_state.command_line_redacted = true;
    }
    shell_state
  }
}

#[derive(Clone)]
pub struct SessionManager {
  inner: Arc<SessionManagerInner>,
}

struct SessionManagerInner {
  registry: Mutex<SessionRegistry>,
  runtime_directory: std::path::PathBuf,
  journal_capacity_bytes: usize,
  checkpoint_interval_bytes: usize,
  ever_had_session: AtomicBool,
  changed: Notify,
}

/// A reporter starts before its child process, while the [`Session`] is only
/// complete after the PTY child and master endpoints have been created. Keep
/// the latest early report until the newly created session can own it.
#[derive(Default)]
struct ShellReportTarget {
  session: Option<std::sync::Weak<Session>>,
  pending_report: Option<ShellReport>,
}

fn deliver_shell_report(target: &Arc<Mutex<ShellReportTarget>>, report: ShellReport) {
  // Keep installation of the session target and delivery of the latest early
  // report in one serialized critical section. Otherwise a newer live report
  // could be applied before a pending startup report, then be overwritten by
  // that stale pending record.
  let mut target = lock(target);
  if let Some(session) = target.session.as_ref().and_then(std::sync::Weak::upgrade) {
    session.apply_shell_report(report);
  } else {
    target.pending_report = Some(report);
  }
}

fn attach_shell_report_target(target: &Arc<Mutex<ShellReportTarget>>, session: &Arc<Session>) {
  let mut target = lock(target);
  target.session = Some(Arc::downgrade(session));
  let pending_report = target.pending_report.take();
  if let Some(report) = pending_report {
    session.apply_shell_report(report);
  }
}

#[derive(Default)]
struct SessionRegistry {
  sessions: HashMap<String, Arc<Session>>,
  pending_names: HashSet<String>,
}

impl SessionManager {
  pub fn new(
    runtime_directory: std::path::PathBuf,
    journal_capacity_bytes: usize,
    checkpoint_interval_bytes: usize,
  ) -> Self {
    Self {
      inner: Arc::new(SessionManagerInner {
        registry: Mutex::new(SessionRegistry::default()),
        runtime_directory,
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
    let shell_report_target = Arc::new(Mutex::new(ShellReportTarget::default()));
    let reporter_target = Arc::clone(&shell_report_target);
    let shell_reporter = ShellReporter::new(&self.inner.runtime_directory, move |report| {
      deliver_shell_report(&reporter_target, report);
    })
    .map_err(SessionManagerError::ShellReporter)?;
    let shell_reporter_path = shell_reporter.path().to_path_buf();

    let pty_system = native_pty_system();
    let pair = pty_system
      .openpty(to_pty_size(&terminal_size))
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let mut command_builder = build_command(command, working_directory);
    command_builder.env("TERM", "xterm-256color");
    command_builder.env("RMUX_SHELL_STATE_PIPE", shell_reporter_path);
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
    let shell_state = ShellState::default();
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
      shell_state: shell_state.clone(),
      alternate_screen: AlternateScreenTracker::default(),
    };
    refresh_checkpoint(&mut terminal);
    let session = Arc::new(Session {
      id: session_id.clone(),
      name,
      created_at_ms: unix_time_ms(),
      terminal: Mutex::new(terminal),
      checkpoint_interval_bytes: self.inner.checkpoint_interval_bytes as u64,
      leases: Mutex::new(AttachmentLeaseRegistry::default()),
      master: Mutex::new(pair.master),
      writer: Mutex::new(writer),
      killer: Mutex::new(killer),
      events,
      shell_state_publisher: ShellStatePublisher::new(shell_state),
      shell_reporter: Mutex::new(Some(shell_reporter)),
    });

    attach_shell_report_target(&shell_report_target, &session);

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
  #[error(transparent)]
  ShellReporter(#[from] ShellReporterError),
  #[error("could not start PTY reader thread: {0}")]
  ReaderThread(std::io::Error),
  #[error("could not start child waiter thread: {0}")]
  WaiterThread(std::io::Error),
}

#[derive(Debug, Error)]
pub enum SessionControlError {
  #[error("this attachment does not own the session input lease")]
  InputLeaseRequired,
  #[error("this attachment does not own the session layout lease")]
  LayoutLeaseRequired,
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

/// Observes only DEC alternate-screen control sequences. This is deliberately
/// narrower than a VT parser: `avt` owns terminal emulation, while rmux only
/// needs a conservative presentation hint for client overlays.
#[derive(Debug, Default)]
struct AlternateScreenTracker {
  parser: CsiObserver,
  active_modes: u8,
  tui_hint: TuiHint,
}

impl AlternateScreenTracker {
  fn observe(&mut self, data: &[u8]) -> Option<TuiHint> {
    if data.is_empty() {
      return None;
    }

    let mut next_hint = if self.tui_hint == TuiHint::Unknown {
      TuiHint::Inline
    } else {
      self.tui_hint
    };
    for byte in data {
      if let Some((enable, modes)) = self.parser.feed(*byte) {
        if enable {
          self.active_modes |= modes;
        } else {
          self.active_modes &= !modes;
        }
        next_hint = if self.active_modes == 0 {
          TuiHint::Inline
        } else {
          TuiHint::AlternateScreen
        };
      }
    }

    if next_hint == self.tui_hint {
      None
    } else {
      self.tui_hint = next_hint;
      Some(next_hint)
    }
  }
}

/// A bounded observer for CSI private-mode set/reset sequences.
///
/// It never claims to interpret arbitrary terminal control data. Unrecognized
/// and malformed sequences simply have no effect on the alternate-screen
/// hint, and a new escape byte restarts an incomplete candidate.
#[derive(Debug, Default)]
struct CsiObserver {
  state: CsiObserverState,
}

#[derive(Debug, Default)]
enum CsiObserverState {
  #[default]
  Ground,
  Escape,
  Csi(Vec<u8>),
}

impl CsiObserver {
  fn feed(&mut self, byte: u8) -> Option<(bool, u8)> {
    match &mut self.state {
      CsiObserverState::Ground => {
        if byte == 0x1b {
          self.state = CsiObserverState::Escape;
        }
        None
      }
      CsiObserverState::Escape => {
        self.state = match byte {
          b'[' => CsiObserverState::Csi(Vec::new()),
          0x1b => CsiObserverState::Escape,
          _ => CsiObserverState::Ground,
        };
        None
      }
      CsiObserverState::Csi(bytes) => {
        if byte == 0x1b {
          self.state = CsiObserverState::Escape;
          return None;
        }

        bytes.push(byte);
        if (0x40..=0x7e).contains(&byte) {
          let sequence = std::mem::take(bytes);
          self.state = CsiObserverState::Ground;
          return alternate_screen_transition(&sequence);
        }

        // Normal private-mode sequences are far shorter than this. Keeping a
        // hard ceiling prevents an arbitrary application stream from turning
        // the observer into an unbounded side buffer.
        if bytes.len() > 64 {
          self.state = CsiObserverState::Ground;
        }
        None
      }
    }
  }
}

fn alternate_screen_transition(sequence: &[u8]) -> Option<(bool, u8)> {
  let (&final_byte, parameters) = sequence.split_last()?;
  if !matches!(final_byte, b'h' | b'l') || !parameters.starts_with(b"?") {
    return None;
  }

  let modes = parameters[1..]
    .split(|byte| *byte == b';')
    .filter_map(|parameter| std::str::from_utf8(parameter).ok())
    .filter_map(|parameter| parameter.parse::<u16>().ok())
    .fold(0_u8, |modes, parameter| {
      modes
        | match parameter {
          47 => 0b001,
          1047 => 0b010,
          1049 => 0b100,
          _ => 0,
        }
    });
  (modes != 0).then_some((final_byte == b'h', modes))
}

fn apply_shell_report_to_terminal(
  terminal: &mut TerminalState,
  report: ShellReport,
) -> Option<ShellState> {
  let ShellReport {
    shell,
    cwd,
    prompt_phase,
    current_command_line,
  } = report;
  // A reporter may be buggy or malicious. Never retain an editable buffer
  // once the shell has left a prompt, even if the private record claims one.
  let current_command_line = match prompt_phase {
    PromptPhase::AtPrompt | PromptPhase::Editing => current_command_line,
    PromptPhase::Unknown | PromptPhase::Running => None,
  };

  let mut candidate = terminal.shell_state.clone();
  candidate.shell = shell;
  candidate.cwd = cwd;
  candidate.prompt_phase = prompt_phase;
  candidate.command_line_redacted = false;
  candidate.current_command_line = current_command_line;
  if !candidate.has_valid_command_line() || candidate == terminal.shell_state {
    return None;
  }

  terminal.shell_state = candidate;
  Some(revise_shell_state(terminal))
}

fn revise_shell_state(terminal: &mut TerminalState) -> ShellState {
  terminal.shell_state.revision = terminal
    .shell_state
    .revision
    .checked_add(1)
    .expect("shell-state revision exhausted");
  terminal.shell_state.observed_sequence = terminal.journal.next_sequence();
  terminal.shell_state.clone()
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
  use std::sync::Arc;
  use std::sync::mpsc;
  use std::thread;

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

  #[test]
  fn alternate_screen_hint_tracks_private_modes_across_chunks() {
    let mut tracker = AlternateScreenTracker::default();

    assert_eq!(tracker.observe(b"prompt"), Some(TuiHint::Inline));
    assert_eq!(tracker.observe(b"\x1b[?10"), None);
    assert_eq!(tracker.observe(b"49h"), Some(TuiHint::AlternateScreen));
    assert_eq!(tracker.observe(b"\x1b[?1049h"), None);
    assert_eq!(tracker.observe(b"\x1b[?47h"), None);
    assert_eq!(tracker.observe(b"\x1b[?1049l"), None);
    assert_eq!(tracker.observe(b"\x1b[?47l"), Some(TuiHint::Inline));
  }

  #[test]
  fn alternate_screen_hint_ignores_non_private_csi_sequences() {
    let mut tracker = AlternateScreenTracker::default();

    assert_eq!(tracker.observe(b"\x1b[2J\x1b[H"), Some(TuiHint::Inline));
    assert_eq!(tracker.observe(b"\x1b[1049h"), None);
    assert_eq!(tracker.observe(b"\x1b[?2004h"), None);
  }

  #[test]
  fn shell_report_tracks_cwd_and_clears_edit_text_when_running() {
    let mut terminal = terminal_state();
    let _output = terminal.journal.append(b"ready");
    let editing = ShellReport {
      shell: rmux_proto::ShellDescriptor {
        shell_type: rmux_proto::ShellType::Zsh,
        integration_version: Some(1),
        capabilities: rmux_proto::ShellCapabilities {
          reports_cwd: true,
          reports_command_line: true,
          reports_cursor: true,
          reports_prompt_phase: true,
        },
      },
      cwd: Some("/workspace".into()),
      prompt_phase: PromptPhase::Editing,
      current_command_line: Some(rmux_proto::CommandLine {
        text: "cargo test".into(),
        cursor_scalar_offset: Some(10),
      }),
    };

    let editing_state = apply_shell_report_to_terminal(&mut terminal, editing)
      .expect("changed shell report should produce a state revision");
    assert_eq!(editing_state.revision, 1);
    assert_eq!(editing_state.observed_sequence, 5);
    assert_eq!(
      editing_state
        .current_command_line
        .as_ref()
        .map(|line| line.text.as_str()),
      Some("cargo test")
    );

    let running = ShellReport {
      shell: editing_state.shell,
      cwd: editing_state.cwd,
      prompt_phase: PromptPhase::Running,
      current_command_line: Some(rmux_proto::CommandLine {
        text: "must not persist".into(),
        cursor_scalar_offset: None,
      }),
    };
    let running_state = apply_shell_report_to_terminal(&mut terminal, running)
      .expect("running state should clear prior editable text");
    assert_eq!(running_state.revision, 2);
    assert_eq!(running_state.current_command_line, None);
  }

  #[test]
  fn serialized_shell_state_publication_never_regresses_the_watch_snapshot() {
    let publisher = Arc::new(ShellStatePublisher::new(ShellState::default()));
    let mut receiver = publisher.subscribe();
    let first_publication = publisher.begin();

    let (ready_sender, ready_receiver) = mpsc::channel();
    let second_publisher = Arc::clone(&publisher);
    let second = thread::spawn(move || {
      ready_sender
        .send(())
        .expect("test should wait for the second publisher");
      let publication = second_publisher.begin();
      let state = ShellState {
        revision: 2,
        ..ShellState::default()
      };
      publication.publish(state);
    });
    ready_receiver
      .recv()
      .expect("second publisher should be ready to contend");

    let first_state = ShellState {
      revision: 1,
      ..ShellState::default()
    };
    first_publication.publish(first_state);
    drop(first_publication);
    second.join().expect("second publisher should complete");

    assert_eq!(receiver.borrow_and_update().revision, 2);
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
      shell_state: ShellState::default(),
      alternate_screen: AlternateScreenTracker::default(),
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
      shell_state: ShellState::default(),
      alternate_screen: AlternateScreenTracker::default(),
    }
  }
}
