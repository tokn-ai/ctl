use crate::process_monitor::ProcessMonitor;
#[cfg(unix)]
use crate::shell_reporter::{ShellReport, ShellReporter, ShellReporterError};
#[cfg(unix)]
use portable_pty::ChildKiller;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use rmux_core::{
  AttachmentLeaseRegistry, AttachmentLeases, JournalError, JournalSnapshot, OutputJournal,
  validate_session_name,
};
use rmux_proto::{
  CommandSpec, CwdSource, ForegroundProcess, LeaseKind, LeaseStatus, PromptPhase, SessionInfo,
  SessionStatus, ShellProcessState, ShellState, TERMINAL_CHECKPOINT_FORMAT,
  TERMINAL_CHECKPOINT_FORMAT_VERSION, TERMINAL_HISTORY_FORMAT, TERMINAL_HISTORY_FORMAT_VERSION,
  TerminalCheckpoint, TerminalHistorySnapshot, TerminalSize, TuiHint,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{Notify, broadcast, watch};
use uuid::Uuid;

const TERMINAL_SCROLLBACK_MAX_ROWS: usize = 10_000;
const TERMINAL_SCROLLBACK_MAX_CELLS: usize = 1_000_000;
// Leave headroom in the 8 MiB protocol frame for line-length prefixes, the
// live checkpoint, and attachment metadata.
const TERMINAL_HISTORY_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum SessionEvent {
  Output,
  /// An authoritative PTY-layout transition placed between raw output ranges.
  PtyGeometryChanged {
    terminal_size: TerminalSize,
    /// The raw-output next offset at the transition.
    observed_sequence: u64,
    /// Internal monotonic ordering for transitions at the same raw boundary.
    geometry_revision: u64,
  },
  Ended {
    exit_code: Option<u32>,
  },
}

pub struct Session {
  id: String,
  name: String,
  created_at_ms: u64,
  terminal: Mutex<TerminalState>,
  checkpoint_interval_bytes: u64,
  leases: Mutex<AttachmentLeaseRegistry>,
  attachments: Mutex<HashMap<String, AttachmentRecord>>,
  master: Mutex<Option<Box<dyn MasterPty + Send>>>,
  writer: Mutex<Box<dyn Write + Send>>,
  #[cfg(unix)]
  killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
  events: broadcast::Sender<SessionEvent>,
  lifecycle: Mutex<SessionLifecycle>,
  shell_state_publisher: ShellStatePublisher,
  #[cfg(unix)]
  shell_reporter: Mutex<Option<ShellReporter>>,
  process_observation_enabled: AtomicBool,
}

struct AttachmentRecord {
  attachment_id: String,
  generation: u64,
  connected: bool,
  superseded: watch::Sender<u64>,
}

pub struct AttachmentRegistration {
  pub attachment_id: String,
  pub attachment_token: String,
  pub generation: u64,
  pub leases: AttachmentLeases,
  pub superseded: watch::Receiver<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLifecycle {
  Running,
  Ended { exit_code: Option<u32> },
}

struct TerminalState {
  terminal: avt::Vt,
  history_control_parser: avt::parser::Parser,
  history_clear_pending: bool,
  history: TerminalHistory,
  /// History captured at exactly the same raw-output boundary as `checkpoint`.
  checkpoint_history: TerminalHistorySnapshot,
  pending_input: Vec<u8>,
  journal: OutputJournal,
  checkpoint: TerminalCheckpoint,
  /// The newest geometry event represented by `checkpoint`.
  checkpoint_geometry_revision: u64,
  terminal_size: TerminalSize,
  /// Monotonically increases for every successful geometry transition. This
  /// is internal ordering state: the raw stream sequence remains the public
  /// protocol boundary.
  geometry_revision: u64,
  /// The most recent emitted geometry boundary, if this session has ever
  /// changed PTY geometry after creation.
  last_geometry_change_sequence: Option<u64>,
  /// A checkpoint captured immediately after `last_geometry_change_sequence`.
  ///
  /// It lets a reconnect at the exact geometry boundary restore the correct
  /// grid without discarding raw output that follows the boundary.
  last_geometry_checkpoint: Option<GeometryCheckpoint>,
  shell_state: ShellState,
  native_cwd: Option<String>,
  reported_cwd: Option<String>,
  alternate_screen: AlternateScreenTracker,
}

struct TerminalHistory {
  generation: u64,
  revision: u64,
  capacity_bytes: usize,
  retained_bytes: usize,
  truncated: bool,
  lines: VecDeque<String>,
}

impl TerminalState {
  fn new(
    terminal_size: TerminalSize,
    journal_capacity_bytes: usize,
    history_capacity_bytes: usize,
  ) -> Self {
    let history = TerminalHistory::new(history_capacity_bytes);
    let mut state = Self {
      terminal: terminal_emulator(&terminal_size),
      history_control_parser: avt::parser::Parser::new(),
      history_clear_pending: false,
      checkpoint_history: history.snapshot(0),
      history,
      pending_input: Vec::new(),
      journal: OutputJournal::new(journal_capacity_bytes),
      checkpoint: TerminalCheckpoint {
        format: TERMINAL_CHECKPOINT_FORMAT.into(),
        format_version: TERMINAL_CHECKPOINT_FORMAT_VERSION,
        sequence: 0,
        terminal_size: terminal_size.clone(),
        payload: Vec::new(),
        input_prefix: Vec::new(),
      },
      checkpoint_geometry_revision: 0,
      terminal_size,
      geometry_revision: 0,
      last_geometry_change_sequence: None,
      last_geometry_checkpoint: None,
      shell_state: ShellState::default(),
      native_cwd: None,
      reported_cwd: None,
      alternate_screen: AlternateScreenTracker::default(),
    };
    refresh_checkpoint(&mut state);
    state
  }
}

impl TerminalHistory {
  fn new(capacity_bytes: usize) -> Self {
    Self {
      generation: 0,
      revision: 0,
      capacity_bytes: capacity_bytes.max(1),
      retained_bytes: 0,
      truncated: false,
      lines: VecDeque::new(),
    }
  }

  fn replace(&mut self, lines: Vec<String>, source_truncated: bool) {
    let mut retained_bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();
    let mut lines = VecDeque::from(lines);
    let mut truncated = self.truncated || source_truncated;
    while retained_bytes > self.capacity_bytes {
      let Some(line) = lines.pop_front() else {
        retained_bytes = 0;
        break;
      };
      retained_bytes = retained_bytes.saturating_sub(line.len() + 1);
      truncated = true;
    }

    if self.lines == lines && self.truncated == truncated {
      return;
    }
    self.lines = lines;
    self.retained_bytes = retained_bytes;
    self.truncated = truncated;
    self.revision = self
      .revision
      .checked_add(1)
      .expect("terminal-history revision exhausted");
  }

  fn clear(&mut self) {
    self.generation = self
      .generation
      .checked_add(1)
      .expect("terminal-history generation exhausted");
    self.revision = self
      .revision
      .checked_add(1)
      .expect("terminal-history revision exhausted");
    self.retained_bytes = 0;
    self.truncated = false;
    self.lines.clear();
  }

  fn snapshot(&self, sequence: u64) -> TerminalHistorySnapshot {
    TerminalHistorySnapshot {
      format: TERMINAL_HISTORY_FORMAT.into(),
      format_version: TERMINAL_HISTORY_FORMAT_VERSION,
      sequence,
      generation: self.generation,
      revision: self.revision,
      retained_bytes: u64::try_from(self.retained_bytes)
        .expect("bounded terminal history byte count fits in u64"),
      truncated: self.truncated,
      lines: self.lines.iter().cloned().collect(),
    }
  }
}

#[derive(Debug, Clone)]
struct GeometryCheckpoint {
  checkpoint: TerminalCheckpoint,
  history: TerminalHistorySnapshot,
  geometry_revision: u64,
}

#[derive(Debug, Clone)]
pub struct AttachSnapshot {
  /// Session metadata captured under the same terminal-state lock as the
  /// journal, checkpoint, and shell snapshot. Later PTY changes are delivered
  /// only as ordered stream events.
  pub session: SessionInfo,
  pub checkpoint: Option<TerminalCheckpoint>,
  /// Internal event ordering state for the checkpoint sent with this snapshot.
  pub checkpoint_geometry_revision: Option<u64>,
  pub journal: JournalSnapshot,
  pub history_gap: bool,
  /// Normalized logical lines that are completely outside the live grid.
  /// Present whenever the attachment also receives a replacing checkpoint.
  pub history: Option<TerminalHistorySnapshot>,
  /// The internal, unredacted state observed atomically with the journal.
  /// Callers must apply their own attachment visibility policy before sending
  /// it to a client.
  pub shell_state: ShellState,
}

/// Couples shell-state revision assignment with terminal-event publication.
///
/// Every caller holds a [`ShellStatePublication`] from before it mutates the
/// terminal state until after it has published the resulting stream event or
/// snapshot. This prevents a delayed older send from overwriting a newer
/// revision in the coalescing watch channel, and serializes raw-output and PTY
/// geometry broadcasts at their shared sequence boundary.
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

  /// Subscribes to terminal events unless the session has already ended.
  ///
  /// The lifecycle check and broadcast subscription share one lock with
  /// [`Self::publish_ended`]. An attachment therefore either subscribes before
  /// the terminal event is broadcast or observes the completed lifecycle; it
  /// can never wait forever after missing `SessionEvent::Ended`.
  pub(crate) fn subscribe_events(&self) -> Result<broadcast::Receiver<SessionEvent>, Option<u32>> {
    let lifecycle = lock(&self.lifecycle);
    let events = self.events.subscribe();
    match *lifecycle {
      SessionLifecycle::Running => Ok(events),
      SessionLifecycle::Ended { exit_code } => Err(exit_code),
    }
  }

  /// Subscribes to the latest complete shell-awareness snapshot.
  ///
  /// A watch channel intentionally coalesces rapid editable-line updates so
  /// they cannot evict raw PTY output from the bounded attachment broadcast.
  pub fn subscribe_shell_state(&self) -> watch::Receiver<ShellState> {
    self.shell_state_publisher.subscribe()
  }

  /// Returns the latest state without live command metadata.
  ///
  /// This is used for non-attachment inspection. A one-shot query has no
  /// input-lease identity to prove that it may observe sensitive command text.
  pub fn shell_state_for_inspection(&self) -> ShellState {
    self.shell_state_for_visibility(false, false)
  }

  /// Returns the latest state filtered for one attachment.
  ///
  /// Editable and running command text are independently opt-in, but each is
  /// visible only when the attachment owns the input lease at the time this
  /// snapshot is produced. A client that has already received text cannot be
  /// made to forget it when it later releases the lease; this policy governs
  /// future snapshots only.
  pub fn shell_state_for_attachment(
    &self,
    attachment_id: &str,
    request_command_line: bool,
    request_running_command: bool,
  ) -> ShellState {
    let owns_input_lease = self.owns_input_lease(attachment_id);
    self.shell_state_for_visibility(
      request_command_line && owns_input_lease,
      request_running_command && owns_input_lease,
    )
  }

  pub fn owns_input_lease(&self, attachment_id: &str) -> bool {
    lock(&self.leases)
      .status(attachment_id, LeaseKind::Input)
      .owned_by_client
  }

  /// Registers a logical attachment and grants any unheld requested leases.
  ///
  /// The returned opaque token can rebind a replacement transport to this
  /// attachment during its bounded reconnect grace period.
  pub fn create_attachment(
    &self,
    request_input_lease: bool,
    request_layout_lease: bool,
  ) -> AttachmentRegistration {
    let attachment_id = Uuid::new_v4().to_string();
    let (superseded, receiver) = watch::channel(0_u64);
    let attachment_token = {
      let mut attachments = lock(&self.attachments);
      let mut candidate = Uuid::new_v4().simple().to_string();
      while attachments.contains_key(&candidate) {
        candidate = Uuid::new_v4().simple().to_string();
      }
      attachments.insert(
        candidate.clone(),
        AttachmentRecord {
          attachment_id: attachment_id.clone(),
          generation: 0,
          connected: true,
          superseded,
        },
      );
      candidate
    };
    let leases =
      lock(&self.leases).request_initial(&attachment_id, request_input_lease, request_layout_lease);
    AttachmentRegistration {
      attachment_id,
      attachment_token,
      generation: 0,
      leases,
      superseded: receiver,
    }
  }

  /// Rebinds a replacement transport and invalidates the previous generation.
  pub fn resume_attachment(&self, attachment_token: &str) -> Option<AttachmentRegistration> {
    let (attachment_id, generation, superseded) = {
      let mut attachments = lock(&self.attachments);
      let record = attachments.get_mut(attachment_token)?;
      record.generation = record.generation.checked_add(1)?;
      record.connected = true;
      let _ignored = record.superseded.send(record.generation);
      (
        record.attachment_id.clone(),
        record.generation,
        record.superseded.subscribe(),
      )
    };
    let leases = lock(&self.leases).attachment_leases(&attachment_id);
    Some(AttachmentRegistration {
      attachment_id,
      attachment_token: attachment_token.into(),
      generation,
      leases,
      superseded,
    })
  }

  /// Marks the current transport generation as temporarily disconnected.
  ///
  /// Returns `true` only when a grace timer should be scheduled.
  pub fn suspend_attachment(&self, attachment_token: &str, generation: u64) -> bool {
    let mut attachments = lock(&self.attachments);
    let Some(record) = attachments.get_mut(attachment_token) else {
      return false;
    };
    if record.generation != generation || !record.connected {
      return false;
    }
    record.connected = false;
    true
  }

  /// Releases a disconnected attachment when its reconnect grace expires.
  pub fn expire_attachment(&self, attachment_token: &str, generation: u64) {
    let attachment_id = {
      let mut attachments = lock(&self.attachments);
      let Some(record) = attachments.get(attachment_token) else {
        return;
      };
      if record.generation != generation || record.connected {
        return;
      }
      attachments
        .remove(attachment_token)
        .map(|record| record.attachment_id)
    };
    if let Some(attachment_id) = attachment_id {
      lock(&self.leases).release_attachment(&attachment_id);
    }
  }

  /// Explicitly detaches the current generation without reconnect grace.
  pub fn close_attachment(&self, attachment_token: &str, generation: u64) {
    let attachment_id = {
      let mut attachments = lock(&self.attachments);
      let Some(record) = attachments.get(attachment_token) else {
        return;
      };
      if record.generation != generation {
        return;
      }
      attachments
        .remove(attachment_token)
        .map(|record| record.attachment_id)
    };
    if let Some(attachment_id) = attachment_id {
      lock(&self.leases).release_attachment(&attachment_id);
    }
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
    self.snapshot_for_delivery(requested, None)
  }

  pub fn snapshot_for_delivery(
    &self,
    requested: Option<u64>,
    checkpoint_geometry_revision: Option<u64>,
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

    let geometry_checkpoint_required = checkpoint_geometry_revision
      .is_none_or(|revision| revision < terminal.geometry_revision)
      && requested.is_some_and(|sequence| {
        terminal
          .last_geometry_change_sequence
          .is_some_and(|boundary| sequence <= boundary)
      });
    let journal_checkpoint_required = requested.is_none_or(|sequence| sequence < earliest_sequence);
    let (checkpoint, checkpoint_history, checkpoint_geometry_revision) =
      if journal_checkpoint_required {
        (
          Some(terminal.checkpoint.clone()),
          Some(terminal.checkpoint_history.clone()),
          Some(terminal.checkpoint_geometry_revision),
        )
      } else if geometry_checkpoint_required {
        // Prefer the checkpoint made at the resize boundary. It is valid only
        // while the journal can still replay immediately after it; otherwise
        // use the newer checkpoint that covers the compacted output as well.
        let geometry_checkpoint = terminal
          .last_geometry_checkpoint
          .as_ref()
          .filter(|checkpoint| checkpoint.checkpoint.sequence >= earliest_sequence)
          .cloned()
          .unwrap_or_else(|| GeometryCheckpoint {
            checkpoint: terminal.checkpoint.clone(),
            history: terminal.checkpoint_history.clone(),
            geometry_revision: terminal.checkpoint_geometry_revision,
          });
        (
          Some(geometry_checkpoint.checkpoint),
          Some(geometry_checkpoint.history),
          Some(geometry_checkpoint.geometry_revision),
        )
      } else {
        (None, None, None)
      };
    let replay_from = checkpoint.as_ref().map_or_else(
      || requested.unwrap_or(earliest_sequence),
      |checkpoint| checkpoint.sequence,
    );
    let journal = terminal.journal.snapshot_from(Some(replay_from))?;
    let history_gap = requested.is_some_and(|sequence| sequence < journal.replay_from)
      || checkpoint_history
        .as_ref()
        .is_some_and(|history| history.truncated);
    Ok(AttachSnapshot {
      session: SessionInfo {
        session_id: self.id.clone(),
        name: self.name.clone(),
        status: SessionStatus::Running,
        created_at_ms: self.created_at_ms,
        next_sequence: terminal.journal.next_sequence(),
        terminal_size: terminal.terminal_size.clone(),
      },
      checkpoint,
      checkpoint_geometry_revision,
      journal,
      // A geometry checkpoint may intentionally advance replay past retained
      // raw bytes, and bounded logical history may have dropped its oldest
      // lines. Report either gap so the client never presents partial history
      // as complete.
      history_gap,
      history: checkpoint_history,
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
    // This gate also covers raw output publication. Holding it while the PTY
    // and terminal parser change makes the geometry event an exact boundary:
    // every earlier raw byte is already broadcast, and later bytes cannot be
    // appended or broadcast until this event has been sent.
    let _publication = self.shell_state_publisher.begin();
    let mut terminal = lock(&self.terminal);
    lock(&self.master)
      .as_ref()
      .ok_or_else(|| SessionControlError::Pty("terminal has closed".into()))?
      .resize(to_pty_size(&terminal_size))
      .map_err(|error| SessionControlError::Pty(error.to_string()))?;

    if terminal.terminal_size != terminal_size {
      terminal.terminal.resize(
        usize::from(terminal_size.columns),
        usize::from(terminal_size.rows),
      );
      terminal.terminal_size = terminal_size.clone();
      terminal.geometry_revision = terminal
        .geometry_revision
        .checked_add(1)
        .expect("PTY geometry revision exhausted");
      refresh_checkpoint(&mut terminal);

      let observed_sequence = terminal.journal.next_sequence();
      terminal.last_geometry_change_sequence = Some(observed_sequence);
      terminal.last_geometry_checkpoint = Some(GeometryCheckpoint {
        checkpoint: terminal.checkpoint.clone(),
        history: terminal.checkpoint_history.clone(),
        geometry_revision: terminal.geometry_revision,
      });
      let _ignored = self.events.send(SessionEvent::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
        geometry_revision: terminal.geometry_revision,
      });
    }

    Ok(())
  }

  pub fn kill(&self) -> Result<(), SessionControlError> {
    #[cfg(unix)]
    lock(&self.killer).kill()?;
    #[cfg(windows)]
    if let Some(master) = lock(&self.master).take() {
      // Closing ConPTY terminates attached console processes, and can block on
      // older Windows versions. The independent PTY reader keeps draining.
      std::thread::Builder::new()
        .name("rmux-conpty-close".into())
        .spawn(move || drop(master))?;
    }
    Ok(())
  }

  fn append_output(&self, data: &[u8]) {
    let publication = self.shell_state_publisher.begin();
    let (chunk, shell_state) = {
      let mut terminal = lock(&self.terminal);
      let alternate_screen = feed_terminal_output(&mut terminal, data);
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
    if chunk.is_some() {
      let _ignored = self.events.send(SessionEvent::Output);
    }
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
  }

  fn publish_ended(&self, exit_code: Option<u32>) {
    self
      .process_observation_enabled
      .store(false, Ordering::Release);
    #[cfg(unix)]
    self.shutdown_shell_reporter();
    let publication = self.shell_state_publisher.begin();
    let shell_state = {
      let mut terminal = lock(&self.terminal);
      if terminal.shell_state.current_command_line.is_none()
        && terminal.shell_state.running_command.is_none()
        && terminal.shell_state.process.is_none()
        && terminal.shell_state.prompt_phase == PromptPhase::Unknown
      {
        None
      } else {
        terminal.shell_state.current_command_line = None;
        terminal.shell_state.command_line_redacted = false;
        terminal.shell_state.running_command = None;
        terminal.shell_state.running_command_redacted = false;
        terminal.shell_state.prompt_phase = PromptPhase::Unknown;
        terminal.shell_state.process = None;
        Some(revise_shell_state(&mut terminal))
      }
    };
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
    let mut lifecycle = lock(&self.lifecycle);
    if matches!(*lifecycle, SessionLifecycle::Ended { .. }) {
      return;
    }
    *lifecycle = SessionLifecycle::Ended { exit_code };
    let _ignored = self.events.send(SessionEvent::Ended { exit_code });
  }

  #[cfg(unix)]
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

  pub(crate) fn process_observation_enabled(&self) -> bool {
    self.process_observation_enabled.load(Ordering::Acquire)
  }

  #[cfg_attr(
    windows,
    allow(
      clippy::unused_self,
      reason = "Windows has no POSIX foreground process group"
    )
  )]
  pub(crate) fn foreground_process_group(&self) -> Option<u32> {
    #[cfg(unix)]
    {
      lock(&self.master)
        .as_ref()
        .and_then(|master| master.process_group_leader())
        .and_then(|pid| u32::try_from(pid).ok())
    }
    #[cfg(windows)]
    {
      None
    }
  }

  pub(crate) fn apply_process_observation(&self, observation: Option<process_info::Snapshot>) {
    let publication = self.shell_state_publisher.begin();
    if !self.process_observation_enabled() {
      return;
    }
    let shell_state = apply_process_observation_to_terminal(&mut lock(&self.terminal), observation);
    if let Some(state) = shell_state {
      publication.publish(state);
    }
  }

  /// Emits a newer complete state snapshot after a connection transitions into
  /// command visibility. The underlying shell report is unchanged, but the
  /// revision keeps every attachment's state stream strictly monotonic.
  pub fn refresh_shell_state_for_visibility(&self) {
    let publication = self.shell_state_publisher.begin();
    let shell_state = {
      let mut terminal = lock(&self.terminal);
      if terminal.shell_state.current_command_line.is_none()
        && terminal.shell_state.running_command.is_none()
      {
        None
      } else {
        Some(revise_shell_state(&mut terminal))
      }
    };
    if let Some(shell_state) = shell_state {
      publication.publish(shell_state);
    }
  }

  #[cfg(unix)]
  fn shutdown_shell_reporter(&self) {
    let reporter = lock(&self.shell_reporter).take();
    if let Some(mut reporter) = reporter {
      let _ignored = reporter.shutdown();
    }
  }

  fn shell_state_for_visibility(
    &self,
    may_view_command_line: bool,
    may_view_running_command: bool,
  ) -> ShellState {
    lock(&self.terminal)
      .shell_state
      .clone()
      .filtered_for_visibility(may_view_command_line, may_view_running_command)
  }
}

#[derive(Clone)]
pub struct SessionManager {
  inner: Arc<SessionManagerInner>,
}

struct SessionManagerInner {
  registry: Mutex<SessionRegistry>,
  #[cfg(unix)]
  runtime_directory: std::path::PathBuf,
  journal_capacity_bytes: usize,
  checkpoint_interval_bytes: usize,
  ever_had_session: AtomicBool,
  changed: Notify,
  process_monitor: Option<ProcessMonitor>,
}

/// A reporter starts before its child process, while the [`Session`] is only
/// complete after the PTY child and master endpoints have been created. Keep
/// the latest early report until the newly created session can own it.
#[derive(Default)]
#[cfg(unix)]
struct ShellReportTarget {
  session: Option<std::sync::Weak<Session>>,
  pending_report: Option<ShellReport>,
}

#[cfg(unix)]
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

#[cfg(unix)]
fn attach_shell_report_target(target: &Arc<Mutex<ShellReportTarget>>, session: &Arc<Session>) {
  let mut target = lock(target);
  target.session = Some(Arc::downgrade(session));
  let pending_report = target.pending_report.take();
  if let Some(report) = pending_report {
    session.apply_shell_report(report);
  }
}

struct SessionRegistry {
  sessions: HashMap<String, Arc<Session>>,
  pending_names: HashSet<String>,
  next_automatic_name: Option<u64>,
}

impl Default for SessionRegistry {
  fn default() -> Self {
    Self {
      sessions: HashMap::new(),
      pending_names: HashSet::new(),
      next_automatic_name: Some(1),
    }
  }
}

impl SessionManager {
  pub fn new(
    runtime_directory: std::path::PathBuf,
    journal_capacity_bytes: usize,
    checkpoint_interval_bytes: usize,
  ) -> Self {
    #[cfg(windows)]
    drop(runtime_directory);
    Self {
      inner: Arc::new(SessionManagerInner {
        registry: Mutex::new(SessionRegistry::default()),
        #[cfg(unix)]
        runtime_directory,
        journal_capacity_bytes,
        checkpoint_interval_bytes: checkpoint_interval_bytes.max(1),
        ever_had_session: AtomicBool::new(false),
        changed: Notify::new(),
        process_monitor: if cfg!(unix) {
          ProcessMonitor::new()
        } else {
          None
        },
      }),
    }
  }

  fn reserve_name(
    &self,
    requested_name: Option<String>,
  ) -> Result<NameReservation, SessionManagerError> {
    match requested_name {
      Some(name) => {
        validate_session_name(&name).map_err(|message| SessionManagerError::InvalidName {
          message: message.into(),
        })?;
        NameReservation::acquire(Arc::clone(&self.inner), name)
      }
      None => NameReservation::acquire_automatic(Arc::clone(&self.inner)),
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
    let reservation = self.reserve_name(requested_name)?;
    let name = reservation.name().to_owned();
    #[cfg(unix)]
    let shell_report_target = Arc::new(Mutex::new(ShellReportTarget::default()));
    #[cfg(unix)]
    let reporter_target = Arc::clone(&shell_report_target);
    #[cfg(unix)]
    let shell_reporter = ShellReporter::new(&self.inner.runtime_directory, move |report| {
      deliver_shell_report(&reporter_target, report);
    })
    .map_err(SessionManagerError::ShellReporter)?;
    #[cfg(unix)]
    let shell_reporter_path = shell_reporter.path().to_path_buf();

    let pty_system = native_pty_system();
    let pair = pty_system
      .openpty(to_pty_size(&terminal_size))
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let mut command_builder = build_command(command, working_directory);
    command_builder.env("TERM", "xterm-256color");
    #[cfg(unix)]
    command_builder.env("RMUX_SHELL_STATE_PIPE", shell_reporter_path);
    let child = pair
      .slave
      .spawn_command(command_builder)
      .map_err(|error| SessionManagerError::Spawn(error.to_string()))?;
    // Capture the birth token while we still own the unreaped child handle.
    // This is a small process-info query; slow cwd/tree work is background-only.
    let process_inspector = child
      .process_id()
      .and_then(|pid| process_info::Inspector::new(pid).ok());
    let reader = pair
      .master
      .try_clone_reader()
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    let writer = pair
      .master
      .take_writer()
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    #[cfg(windows)]
    let crate::conpty::PtyIo {
      reader,
      writer,
      initial_output,
    } = crate::conpty::initialize(reader, writer)
      .map_err(|error| SessionManagerError::Pty(error.to_string()))?;
    #[cfg(unix)]
    let killer = child.clone_killer();
    drop(pair.slave);

    let (events, _) = broadcast::channel(256);
    let terminal = TerminalState::new(
      terminal_size,
      self.inner.journal_capacity_bytes,
      TERMINAL_HISTORY_CAPACITY_BYTES,
    );
    let shell_state = terminal.shell_state.clone();
    let session = Arc::new(Session {
      id: session_id.clone(),
      name,
      created_at_ms: unix_time_ms(),
      terminal: Mutex::new(terminal),
      checkpoint_interval_bytes: self.inner.checkpoint_interval_bytes as u64,
      leases: Mutex::new(AttachmentLeaseRegistry::default()),
      attachments: Mutex::new(HashMap::new()),
      master: Mutex::new(Some(pair.master)),
      writer: Mutex::new(writer),
      #[cfg(unix)]
      killer: Mutex::new(killer),
      events,
      lifecycle: Mutex::new(SessionLifecycle::Running),
      shell_state_publisher: ShellStatePublisher::new(shell_state),
      #[cfg(unix)]
      shell_reporter: Mutex::new(Some(shell_reporter)),
      process_observation_enabled: AtomicBool::new(process_inspector.is_some()),
    });

    #[cfg(windows)]
    if !initial_output.is_empty() {
      session.append_output(&initial_output);
    }
    #[cfg(unix)]
    attach_shell_report_target(&shell_report_target, &session);
    if let (Some(monitor), Some(inspector)) = (&self.inner.process_monitor, process_inspector) {
      monitor.register(inspector, &session);
    }

    reservation.commit(session_id.clone(), Arc::clone(&session));
    self.inner.ever_had_session.store(true, Ordering::Release);
    self.inner.changed.notify_one();

    start_session_workers(child, reader, &session, &self.inner)?;

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

  /// Returns a stable snapshot of live sessions for a daemon-wide lifecycle
  /// transition.
  ///
  /// Callers must hold the transition gate that serializes this snapshot with
  /// [`Self::create`]. The returned `Arc`s keep selected sessions alive while
  /// their termination is requested outside the registry lock.
  pub(crate) fn snapshot_for_cooperative_restart(&self) -> Vec<Arc<Session>> {
    lock(&self.inner.registry)
      .sessions
      .values()
      .cloned()
      .collect()
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

  fn acquire_automatic(manager: Arc<SessionManagerInner>) -> Result<Self, SessionManagerError> {
    let name = {
      let mut registry = lock(&manager.registry);
      loop {
        let sequence = registry
          .next_automatic_name
          .ok_or(SessionManagerError::AutomaticNameExhausted)?;
        registry.next_automatic_name = sequence.checked_add(1);
        let candidate = format!("session-{sequence}");
        let exists = registry.pending_names.contains(&candidate)
          || registry
            .sessions
            .values()
            .any(|session| session.name == candidate);
        if !exists {
          registry.pending_names.insert(candidate.clone());
          break candidate;
        }
      }
    };

    Ok(Self {
      manager,
      name,
      active: true,
    })
  }

  fn name(&self) -> &str {
    &self.name
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

fn start_session_workers(
  mut child: Box<dyn portable_pty::Child + Send + Sync>,
  reader: Box<dyn Read + Send>,
  session: &Arc<Session>,
  manager: &Arc<SessionManagerInner>,
) -> Result<(), SessionManagerError> {
  let session_id = session.id.clone();
  let reader_session = Arc::clone(session);
  let reader_thread = std::thread::Builder::new()
    .name(format!("rmux-reader-{}", &session_id[..8]))
    .spawn(move || read_pty(reader, &reader_session))
    .map_err(SessionManagerError::ReaderThread)?;

  let manager = Arc::downgrade(manager);
  let waiter_session = Arc::clone(session);
  std::thread::Builder::new()
    .name(format!("rmux-waiter-{}", &session_id[..8]))
    .spawn(move || {
      let exit_code = child.wait().ok().map(|status| status.exit_code());
      // ConPTY keeps the output pipe open until the pseudoconsole closes.
      // Drop it on this waiter thread while the reader continues to drain.
      #[cfg(windows)]
      {
        let master = lock(&waiter_session.master).take();
        drop(master);
      }
      let _reader_result = reader_thread.join();
      waiter_session.publish_ended(exit_code);
      if let Some(manager) = manager.upgrade() {
        lock(&manager.registry).sessions.remove(&session_id);
        manager.changed.notify_one();
      }
    })
    .map_err(SessionManagerError::WaiterThread)?;

  Ok(())
}

#[derive(Debug, Error)]
pub enum SessionManagerError {
  #[error("invalid session name: {message}")]
  InvalidName { message: String },
  #[error("a session named '{name}' already exists")]
  AlreadyExists { name: String },
  #[error("automatic session name sequence is exhausted")]
  AutomaticNameExhausted,
  #[error("session '{selector}' was not found")]
  NotFound { selector: String },
  #[error("could not create PTY: {0}")]
  Pty(String),
  #[error("could not spawn child process: {0}")]
  Spawn(String),
  #[error(transparent)]
  #[cfg(unix)]
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
  fn active(&self) -> bool {
    self.active_modes != 0
  }

  fn reset(&mut self) -> Option<TuiHint> {
    let changed = self.tui_hint != TuiHint::Inline;
    *self = Self {
      tui_hint: TuiHint::Inline,
      ..Self::default()
    };
    changed.then_some(TuiHint::Inline)
  }

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

#[cfg(unix)]
fn apply_shell_report_to_terminal(
  terminal: &mut TerminalState,
  report: ShellReport,
) -> Option<ShellState> {
  let ShellReport {
    shell,
    cwd,
    prompt_phase,
    current_command_line,
    running_command,
  } = report;
  // A reporter may be buggy or malicious. The two active text forms are
  // phase-exclusive: never retain an editable buffer outside a prompt or a
  // running summary outside the running phase.
  let (current_command_line, running_command) = match prompt_phase {
    PromptPhase::AtPrompt | PromptPhase::Editing => (current_command_line, None),
    PromptPhase::Running => (None, running_command),
    PromptPhase::Unknown => (None, None),
  };

  let mut candidate = terminal.shell_state.clone();
  candidate.shell = shell;
  set_observed_cwd(&mut candidate, cwd.as_ref(), terminal.native_cwd.as_ref());
  candidate.prompt_phase = prompt_phase;
  candidate.command_line_redacted = false;
  candidate.current_command_line = current_command_line;
  candidate.running_command_redacted = false;
  candidate.running_command = running_command;
  if !candidate.has_valid_metadata() {
    return None;
  }
  terminal.reported_cwd = cwd;
  if candidate == terminal.shell_state {
    return None;
  }

  terminal.shell_state = candidate;
  Some(revise_shell_state(terminal))
}

fn set_observed_cwd(state: &mut ShellState, reported: Option<&String>, native: Option<&String>) {
  let (cwd, source) = if let Some(cwd) = reported {
    (Some(cwd.clone()), Some(CwdSource::ShellIntegration))
  } else if let Some(cwd) = native {
    (Some(cwd.clone()), Some(CwdSource::Process))
  } else {
    (None, None)
  };
  state.cwd_display = cwd.as_deref().map(display_working_directory);
  state.cwd = cwd;
  state.cwd_source = source;
}

fn apply_process_observation_to_terminal(
  terminal: &mut TerminalState,
  observation: Option<process_info::Snapshot>,
) -> Option<ShellState> {
  let mut candidate = terminal.shell_state.clone();
  let (native_cwd, process) = observation.map_or((None, None), |observation| {
    let cwd = observation
      .cwd
      .and_then(|path| path.into_os_string().into_string().ok());
    let foreground = match observation.foreground {
      process_info::Foreground::Unknown => ForegroundProcess::Unknown,
      process_info::Foreground::Shell => ForegroundProcess::Shell,
      process_info::Foreground::Child(process) => ForegroundProcess::Child {
        pid: process.identity.pid,
        name: process.name,
      },
    };
    (
      cwd,
      Some(ShellProcessState {
        pid: observation.shell.identity.pid,
        name: observation.shell.name,
        foreground,
      }),
    )
  });
  candidate.process = process;
  set_observed_cwd(
    &mut candidate,
    terminal.reported_cwd.as_ref(),
    native_cwd.as_ref(),
  );
  if !candidate.has_valid_metadata() {
    return None;
  }
  terminal.native_cwd = native_cwd;
  if candidate == terminal.shell_state {
    return None;
  }
  terminal.shell_state = candidate;
  Some(revise_shell_state(terminal))
}

fn display_working_directory(cwd: &str) -> String {
  target_home_directory()
    .as_deref()
    .and_then(|home| abbreviate_home_directory(cwd, home))
    .unwrap_or_else(|| cwd.into())
}

fn target_home_directory() -> Option<std::ffi::OsString> {
  #[cfg(unix)]
  const HOME_ENVIRONMENT: &str = "HOME";
  #[cfg(windows)]
  const HOME_ENVIRONMENT: &str = "USERPROFILE";
  #[cfg(not(any(unix, windows)))]
  const HOME_ENVIRONMENT: &str = "HOME";

  std::env::var_os(HOME_ENVIRONMENT)
}

fn abbreviate_home_directory(path: &str, home: &OsStr) -> Option<String> {
  let path = Path::new(path);
  let home = Path::new(home);
  if home.as_os_str().is_empty() || path.is_absolute() != home.is_absolute() {
    return None;
  }

  let relative = path.strip_prefix(home).ok()?;
  if relative.as_os_str().is_empty() {
    return Some("~".into());
  }

  Path::new("~")
    .join(relative)
    .into_os_string()
    .into_string()
    .ok()
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
  refresh_history(terminal);
  terminal.checkpoint = TerminalCheckpoint {
    format: TERMINAL_CHECKPOINT_FORMAT.into(),
    format_version: TERMINAL_CHECKPOINT_FORMAT_VERSION,
    sequence: terminal.journal.next_sequence(),
    terminal_size: terminal.terminal_size.clone(),
    payload: terminal.terminal.dump().into_bytes(),
    input_prefix: terminal.pending_input.clone(),
  };
  terminal.checkpoint_history = terminal.history.snapshot(terminal.checkpoint.sequence);
  terminal.checkpoint_geometry_revision = terminal.geometry_revision;
}

fn refresh_history(terminal: &mut TerminalState) {
  if terminal.alternate_screen.active() {
    return;
  }
  let source_truncated = terminal
    .terminal
    .lines()
    .count()
    .saturating_sub(usize::from(terminal.terminal_size.rows))
    >= terminal_scrollback_rows(&terminal.terminal_size);
  let lines = logical_history_lines(&terminal.terminal, &terminal.terminal_size);
  terminal.history.replace(lines, source_truncated);
}

fn logical_history_lines(terminal: &avt::Vt, terminal_size: &TerminalSize) -> Vec<String> {
  let mut all_lines = terminal.text();
  let mut live_terminal = terminal_emulator(terminal_size);
  live_terminal.feed_str(&terminal.dump());
  let live_lines = live_terminal.text();

  let mut matching_suffix = 0;
  while matching_suffix < all_lines.len()
    && matching_suffix < live_lines.len()
    && all_lines[all_lines.len() - 1 - matching_suffix]
      == live_lines[live_lines.len() - 1 - matching_suffix]
  {
    matching_suffix += 1;
  }

  let mut history_end = all_lines.len().saturating_sub(matching_suffix);
  if matching_suffix < live_lines.len() && history_end > 0 {
    let active_prefix = &all_lines[history_end - 1];
    let visible_suffix = &live_lines[live_lines.len() - 1 - matching_suffix];
    if active_prefix.ends_with(visible_suffix) {
      history_end -= 1;
    } else {
      // A checkpoint dump must describe a suffix of the authoritative primary
      // buffer. If a future emulator format violates that invariant, omit
      // history rather than duplicating mutable screen content as scrollback.
      history_end = 0;
    }
  }
  all_lines.truncate(history_end);
  all_lines
}

fn terminal_emulator(terminal_size: &TerminalSize) -> avt::Vt {
  // rmuxd owns a bounded authoritative scrollback in addition to its live
  // checkpoint. Raw output remains a short delta journal, not history state.
  avt::Vt::builder()
    .size(
      usize::from(terminal_size.columns),
      usize::from(terminal_size.rows),
    )
    .scrollback_limit(terminal_scrollback_rows(terminal_size))
    .build()
}

fn terminal_scrollback_rows(terminal_size: &TerminalSize) -> usize {
  let columns = usize::from(terminal_size.columns).max(1);
  TERMINAL_SCROLLBACK_MAX_ROWS.min(TERMINAL_SCROLLBACK_MAX_CELLS / columns)
}

fn feed_terminal_output(terminal: &mut TerminalState, data: &[u8]) -> Option<TuiHint> {
  feed_terminal_bytes_inner(terminal, data)
}

#[cfg(all(test, unix))]
fn feed_terminal_bytes(terminal: &mut TerminalState, data: &[u8]) {
  let _ignored = feed_terminal_bytes_inner(terminal, data);
}

fn feed_terminal_bytes_inner(terminal: &mut TerminalState, data: &[u8]) -> Option<TuiHint> {
  let mut tui_hint = None;
  terminal.pending_input.extend_from_slice(data);

  loop {
    match std::str::from_utf8(&terminal.pending_input) {
      Ok(valid) => {
        let valid = valid.to_owned();
        tui_hint = feed_terminal_text(terminal, &valid).or(tui_hint);
        terminal.pending_input.clear();
        break;
      }
      Err(error) => {
        let valid_up_to = error.valid_up_to();
        if valid_up_to > 0 {
          let valid = String::from_utf8(terminal.pending_input[..valid_up_to].to_vec())
            .expect("valid UTF-8 prefix reported by std::str::from_utf8");
          tui_hint = feed_terminal_text(terminal, &valid).or(tui_hint);
          terminal.pending_input.drain(..valid_up_to);
          continue;
        }

        let Some(invalid_length) = error.error_len() else {
          break;
        };
        tui_hint = feed_terminal_character(terminal, '\u{fffd}').or(tui_hint);
        terminal.pending_input.drain(..invalid_length);
      }
    }
  }

  tui_hint
}

fn feed_terminal_text(terminal: &mut TerminalState, text: &str) -> Option<TuiHint> {
  let mut tui_hint = None;
  for ch in text.chars() {
    tui_hint = feed_terminal_character(terminal, ch).or(tui_hint);
  }
  // `Vt::feed` deliberately defers dirty-line collection and scrollback GC;
  // an empty batch performs that bounded maintenance once per PTY read.
  terminal.terminal.feed_str("");
  tui_hint
}

fn feed_terminal_character(terminal: &mut TerminalState, ch: char) -> Option<TuiHint> {
  use avt::parser::{EdScope, Function};

  let mut encoded = [0; 4];
  let tui_hint = terminal
    .alternate_screen
    .observe(ch.encode_utf8(&mut encoded).as_bytes());
  let history_action = match terminal.history_control_parser.feed(ch) {
    Some(Function::Ed(EdScope::SavedLines)) => Some(false),
    Some(Function::Ris) => Some(true),
    _ => None,
  };
  terminal.terminal.feed(ch);

  let Some(terminal_already_reset) = history_action else {
    apply_pending_history_clear(terminal);
    return tui_hint;
  };
  terminal.history.clear();
  if terminal_already_reset {
    terminal.history_clear_pending = false;
    return terminal.alternate_screen.reset().or(tui_hint);
  }
  terminal.history_clear_pending = true;
  apply_pending_history_clear(terminal);
  tui_hint
}

fn apply_pending_history_clear(terminal: &mut TerminalState) {
  if !terminal.history_clear_pending || terminal.alternate_screen.active() {
    return;
  }

  // AVT currently parses ED 3 but intentionally leaves saved lines intact.
  // Replaying its primary-screen dump into a fresh bounded emulator preserves
  // the live view and parser prefix while dropping only the old scrollback.
  let payload = terminal.terminal.dump();
  let mut replacement = terminal_emulator(&terminal.terminal_size);
  replacement.feed_str(&payload);
  terminal.terminal = replacement;
  terminal.history_clear_pending = false;
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

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::sync::mpsc;
  use std::sync::{Arc, Barrier};
  use std::thread;

  #[test]
  fn automatic_names_are_monotonic_and_safe_under_concurrent_reservations() {
    const AUTOMATIC_RESERVATIONS: u64 = 16;

    let manager = SessionManager::new(std::path::PathBuf::new(), 1, 1);
    let explicit = NameReservation::acquire(Arc::clone(&manager.inner), "session-7".into())
      .expect("the explicit name should be reserved");
    let barrier = Arc::new(Barrier::new(
      usize::try_from(AUTOMATIC_RESERVATIONS).expect("test count fits in usize") + 1,
    ));
    let mut handles = Vec::new();

    for _ in 0..AUTOMATIC_RESERVATIONS {
      let inner = Arc::clone(&manager.inner);
      let barrier = Arc::clone(&barrier);
      handles.push(thread::spawn(move || {
        let reservation = NameReservation::acquire_automatic(inner)
          .expect("automatic names should remain available");
        barrier.wait();
        reservation
      }));
    }

    barrier.wait();
    let names: HashSet<_> = handles
      .into_iter()
      .map(|handle| {
        let reservation = handle
          .join()
          .expect("name reservation thread should complete");
        reservation.name().to_owned()
      })
      .collect();
    let expected: HashSet<_> = (1..=AUTOMATIC_RESERVATIONS + 1)
      .filter(|sequence| *sequence != 7)
      .map(|sequence| format!("session-{sequence}"))
      .collect();
    assert_eq!(names, expected);

    drop(explicit);
    let next = NameReservation::acquire_automatic(Arc::clone(&manager.inner))
      .expect("released names must not rewind the automatic sequence");
    assert_eq!(next.name(), "session-18");
  }

  #[test]
  fn automatic_names_use_the_final_sequence_before_exhaustion() {
    let manager = SessionManager::new(std::path::PathBuf::new(), 1, 1);
    lock(&manager.inner.registry).next_automatic_name = Some(u64::MAX);

    let final_name = NameReservation::acquire_automatic(Arc::clone(&manager.inner))
      .expect("the final automatic name should remain usable");
    assert_eq!(final_name.name(), format!("session-{}", u64::MAX));
    drop(final_name);

    assert!(matches!(
      NameReservation::acquire_automatic(Arc::clone(&manager.inner)),
      Err(SessionManagerError::AutomaticNameExhausted)
    ));
  }

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
  fn checkpoint_parser_retains_bounded_scrollback_behind_the_live_view() {
    let mut terminal = terminal_state();
    let rows = usize::from(terminal.terminal_size.rows);

    for _ in 0..(rows + 10) {
      feed_terminal_bytes(&mut terminal, b"line\r\n");
    }

    assert_eq!(terminal.terminal.view().count(), rows);
    assert!(terminal.terminal.lines().count() > rows);
  }

  #[test]
  fn history_contains_only_complete_logical_lines_above_the_live_view() {
    let mut terminal = terminal_state_with_size(5, 2, 1024);

    feed_terminal_bytes(&mut terminal, b"first\r\nsecond\r\nlive");
    refresh_history(&mut terminal);

    assert_eq!(terminal.history.snapshot(0).lines, vec!["first"]);
  }

  #[test]
  fn soft_wrapped_history_is_one_logical_line_across_resize() {
    let mut terminal = terminal_state_with_size(5, 2, 1024);

    feed_terminal_bytes(&mut terminal, b"abcdefghij\r\none\r\ntwo");
    refresh_history(&mut terminal);
    assert_eq!(terminal.history.snapshot(0).lines, vec!["abcdefghij"]);

    terminal.terminal.resize(10, 2);
    terminal.terminal_size.columns = 10;
    refresh_history(&mut terminal);
    assert_eq!(terminal.history.snapshot(0).lines, vec!["abcdefghij"]);
  }

  #[test]
  fn alternate_screen_output_does_not_replace_primary_history() {
    let mut terminal = terminal_state_with_size(8, 2, 1024);
    feed_terminal_bytes(&mut terminal, b"history\r\nprimary\r\nlive");
    refresh_history(&mut terminal);
    let primary_history = terminal.history.snapshot(0);

    feed_terminal_output(&mut terminal, b"\x1b[?1049halternate\r\ncontent\r\nmore");
    refresh_history(&mut terminal);
    assert_eq!(terminal.history.snapshot(0), primary_history);

    feed_terminal_output(&mut terminal, b"\x1b[?1049l");
    refresh_history(&mut terminal);
    assert_eq!(terminal.history.snapshot(0), primary_history);
  }

  #[test]
  fn erase_saved_lines_starts_a_new_history_generation() {
    let mut terminal = terminal_state_with_size(8, 2, 1024);
    feed_terminal_bytes(&mut terminal, b"history\r\nprimary\r\nlive");
    refresh_checkpoint(&mut terminal);
    let old_checkpoint_history = terminal.checkpoint_history.clone();
    assert_eq!(old_checkpoint_history.lines, vec!["history"]);

    feed_terminal_bytes(&mut terminal, b"\x1b[3J");
    refresh_history(&mut terminal);

    let history = terminal.history.snapshot(0);
    assert_eq!(history.generation, old_checkpoint_history.generation + 1);
    assert!(history.lines.is_empty());
    assert_eq!(terminal.checkpoint_history, old_checkpoint_history);
    assert_eq!(terminal.terminal.lines().count(), 2);
  }

  #[test]
  fn output_after_erasing_saved_lines_in_the_same_chunk_is_retained() {
    let mut terminal = terminal_state_with_size(12, 2, 1024);
    feed_terminal_bytes(&mut terminal, b"old-history\r\nprimary\r\nlive");
    refresh_history(&mut terminal);
    assert_eq!(terminal.history.snapshot(0).lines, vec!["old-history"]);

    feed_terminal_bytes(
      &mut terminal,
      b"\x1b[3J\x1b[2J\x1b[Hnew-history\r\nnew-screen\r\nlive",
    );
    refresh_history(&mut terminal);

    assert_eq!(terminal.history.snapshot(0).lines, vec!["new-history"]);
  }

  #[test]
  fn bounded_history_discards_whole_oldest_lines() {
    let mut history = TerminalHistory::new(8);

    history.replace(vec!["one".into(), "two".into(), "three".into()], false);
    let first = history.snapshot(0);
    assert_eq!(first.lines, vec!["three"]);
    assert_eq!(first.retained_bytes, 6);
    assert!(first.truncated);

    history.replace(vec!["three".into()], false);
    assert_eq!(history.snapshot(0), first);

    history.clear();
    let cleared = history.snapshot(0);
    assert_eq!(cleared.generation, first.generation + 1);
    assert!(!cleared.truncated);
    assert!(cleared.lines.is_empty());
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
  fn shell_report_tracks_cwd_and_replaces_edit_text_with_running_summary() {
    let mut terminal = terminal_state();
    let _output = terminal.journal.append(b"ready");
    let editing = ShellReport {
      shell: rmux_proto::ShellDescriptor {
        shell_type: rmux_proto::ShellType::Zsh,
        integration_version: Some(2),
        capabilities: rmux_proto::ShellCapabilities {
          reports_cwd: true,
          reports_command_line: true,
          reports_cursor: true,
          reports_prompt_phase: true,
          reports_running_command: true,
        },
      },
      cwd: Some("/workspace".into()),
      prompt_phase: PromptPhase::Editing,
      current_command_line: Some(rmux_proto::CommandLine {
        text: "cargo test".into(),
        cursor_scalar_offset: Some(10),
      }),
      running_command: None,
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
      current_command_line: None,
      running_command: Some("cargo test --workspace".into()),
    };
    let running_state = apply_shell_report_to_terminal(&mut terminal, running)
      .expect("running state should clear prior editable text");
    assert_eq!(running_state.revision, 2);
    assert_eq!(running_state.current_command_line, None);
    assert_eq!(
      running_state.running_command.as_deref(),
      Some("cargo test --workspace")
    );
  }

  #[test]
  fn native_observations_provide_cwd_and_job_without_inventing_shell_reports() {
    let mut terminal = terminal_state();
    let _output = terminal.journal.append(b"ready");
    let sample = native_sample("/native");
    let state = apply_process_observation_to_terminal(&mut terminal, Some(sample.clone())).unwrap();
    assert_eq!(state.revision, 1);
    assert_eq!(state.observed_sequence, 5);
    assert_eq!(state.cwd.as_deref(), Some("/native"));
    assert_eq!(state.cwd_source, Some(CwdSource::Process));
    assert_eq!(state.shell, rmux_proto::ShellDescriptor::default());
    assert_eq!(state.prompt_phase, PromptPhase::Unknown);
    assert_eq!(state.current_command_line, None);
    assert_eq!(state.running_command, None);
    assert_eq!(
      state.process.unwrap().foreground,
      ForegroundProcess::Child {
        pid: 20,
        name: Some("sleep".into())
      }
    );
    assert_eq!(
      apply_process_observation_to_terminal(&mut terminal, Some(sample)),
      None
    );
    assert_eq!(terminal.shell_state.revision, 1);

    let unavailable = apply_process_observation_to_terminal(&mut terminal, None).unwrap();
    assert_eq!(unavailable.cwd, None);
    assert_eq!(unavailable.cwd_source, None);
    assert_eq!(unavailable.process, None);
    assert_eq!(unavailable.revision, 2);
  }

  #[test]
  fn shell_reports_win_over_native_cwd_and_native_failures_preserve_reports() {
    let mut terminal = terminal_state();
    apply_process_observation_to_terminal(&mut terminal, Some(native_sample("/physical")));
    let state =
      apply_shell_report_to_terminal(&mut terminal, cwd_report(Some("/logical"))).unwrap();
    assert_eq!(state.cwd.as_deref(), Some("/logical"));
    assert_eq!(state.cwd_source, Some(CwdSource::ShellIntegration));
    assert!(state.process.is_some());
    let revision = state.revision;
    assert_eq!(
      apply_process_observation_to_terminal(&mut terminal, Some(native_sample("/new-physical"))),
      None
    );
    assert_eq!(terminal.native_cwd.as_deref(), Some("/new-physical"));
    assert_eq!(terminal.shell_state.revision, revision);

    let missing = apply_process_observation_to_terminal(&mut terminal, None).unwrap();
    assert_eq!(missing.cwd.as_deref(), Some("/logical"));
    assert_eq!(missing.cwd_source, Some(CwdSource::ShellIntegration));
    assert_eq!(missing.prompt_phase, PromptPhase::AtPrompt);
    assert_eq!(missing.process, None);
  }

  #[test]
  fn reports_without_cwd_use_the_latest_native_fallback() {
    let mut terminal = terminal_state();
    apply_shell_report_to_terminal(&mut terminal, cwd_report(Some("/logical")));
    apply_process_observation_to_terminal(&mut terminal, Some(native_sample("/physical")));
    let state = apply_shell_report_to_terminal(&mut terminal, cwd_report(None)).unwrap();
    assert_eq!(state.cwd.as_deref(), Some("/physical"));
    assert_eq!(state.cwd_source, Some(CwdSource::Process));
    assert_eq!(state.prompt_phase, PromptPhase::AtPrompt);
  }

  fn native_sample(cwd: &str) -> process_info::Snapshot {
    let shell = process_info::ProcessInfo {
      identity: process_info::ProcessIdentity {
        pid: 10,
        start_time: 1,
      },
      parent_pid: 1,
      process_group: 10,
      name: Some("sh".into()),
    };
    process_info::Snapshot {
      shell,
      cwd: Some(cwd.into()),
      foreground: process_info::Foreground::Child(process_info::ProcessInfo {
        identity: process_info::ProcessIdentity {
          pid: 20,
          start_time: 2,
        },
        parent_pid: 10,
        process_group: 20,
        name: Some("sleep".into()),
      }),
    }
  }

  fn cwd_report(cwd: Option<&str>) -> ShellReport {
    ShellReport {
      shell: rmux_proto::ShellDescriptor {
        shell_type: rmux_proto::ShellType::Sh,
        integration_version: Some(1),
        capabilities: rmux_proto::ShellCapabilities {
          reports_cwd: true,
          reports_prompt_phase: true,
          ..rmux_proto::ShellCapabilities::default()
        },
      },
      cwd: cwd.map(str::to_owned),
      prompt_phase: PromptPhase::AtPrompt,
      current_command_line: None,
      running_command: None,
    }
  }

  #[test]
  fn working_directory_display_abbreviates_only_the_home_path_boundary() {
    assert_eq!(
      abbreviate_home_directory("/Users/me", OsStr::new("/Users/me")),
      Some("~".into())
    );
    assert_eq!(
      abbreviate_home_directory("/Users/me/project", OsStr::new("/Users/me")),
      Some("~/project".into())
    );
    assert_eq!(
      abbreviate_home_directory("/Users/meanwhile", OsStr::new("/Users/me")),
      None
    );
    assert_eq!(
      abbreviate_home_directory("/work/project", OsStr::new("/Users/me")),
      None
    );
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
    terminal_state_with_size(
      terminal_size.columns,
      terminal_size.rows,
      TERMINAL_HISTORY_CAPACITY_BYTES,
    )
  }

  fn terminal_state_with_size(
    columns: u16,
    rows: u16,
    history_capacity_bytes: usize,
  ) -> TerminalState {
    let terminal_size = TerminalSize {
      columns,
      rows,
      ..TerminalSize::default()
    };
    TerminalState::new(terminal_size, 1024, history_capacity_bytes)
  }

  fn terminal_state_from_checkpoint(checkpoint: TerminalCheckpoint) -> TerminalState {
    let mut terminal = terminal_emulator(&checkpoint.terminal_size);
    let payload = String::from_utf8(checkpoint.payload.clone())
      .expect("checkpoint payload is generated from a UTF-8 VT dump");
    terminal.feed_str(&payload);
    TerminalState {
      terminal,
      history_control_parser: avt::parser::Parser::new(),
      history_clear_pending: false,
      history: TerminalHistory::new(TERMINAL_HISTORY_CAPACITY_BYTES),
      checkpoint_history: TerminalHistory::new(TERMINAL_HISTORY_CAPACITY_BYTES).snapshot(0),
      pending_input: checkpoint.input_prefix.clone(),
      journal: OutputJournal::new(1024),
      terminal_size: checkpoint.terminal_size.clone(),
      checkpoint,
      checkpoint_geometry_revision: 0,
      geometry_revision: 0,
      last_geometry_change_sequence: None,
      last_geometry_checkpoint: None,
      shell_state: ShellState::default(),
      native_cwd: None,
      reported_cwd: None,
      alternate_screen: AlternateScreenTracker::default(),
    }
  }
}
