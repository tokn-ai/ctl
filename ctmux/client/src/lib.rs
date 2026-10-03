pub mod archive;
pub mod cache;
pub mod history;
mod history_sync;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
pub use ctmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES;
use ctmux_proto::{
  ClientMessage, CodecError, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, ShellState, TerminalCheckpoint, TerminalHistoryManifest, TerminalHistoryRow,
  TerminalHistorySnapshot, TerminalSize, read_frame, write_frame,
};
use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at};

const DETACH_BYTE: u8 = 0x1d;
// `ctmux_vt_state` is an initialization stream, not an idempotent patch. The
// raw terminal presenter therefore starts every restore from a defined state.
const CHECKPOINT_RENDERER_RESET: &[u8] = b"\x1bc\x1b[2J\x1b[H";

/// Stable metadata sent during the `ctmux` protocol handshake.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
  pub name: String,
  pub version: String,
}

/// Parameters for opening an attachment over an already-selected transport.
#[allow(
  clippy::struct_excessive_bools,
  reason = "each flag mirrors an independently negotiated attachment capability"
)]
#[derive(Debug, Clone)]
pub struct AttachRequest {
  pub session: String,
  pub resume_from: Option<u64>,
  pub terminal_size: TerminalSize,
  pub request_input_lease: bool,
  pub request_layout_lease: bool,
  /// Request the current editable command line when daemon policy allows it.
  pub request_command_line: bool,
  /// Request the current non-editable running-command summary when daemon
  /// policy allows it.
  pub request_running_command: bool,
  /// Maximum raw output ctmuxd may send beyond renderer-applied state.
  pub presentation_window_bytes: u64,
}

/// Session metadata and attachment-relative state returned by `ctmuxd`.
#[derive(Debug, Clone)]
pub struct AttachedSession {
  /// Version metadata from this exact transport handshake.
  pub handshake_info: HandshakeInfo,
  /// Opaque credential for rebinding this logical attachment after an
  /// unexpected transport loss.
  pub attachment_token: String,
  pub session: SessionInfo,
  pub replay_from: u64,
  pub history_gap: bool,
  pub checkpoint: Option<TerminalCheckpoint>,
  pub history: Option<TerminalHistorySnapshot>,
  pub history_manifest: Option<TerminalHistoryManifest>,
  pub terminal_size_mismatch: bool,
  pub input_lease: LeaseStatus,
  pub layout_lease: LeaseStatus,
  /// Complete shell-awareness state as it existed when the attachment opened.
  ///
  /// Use [`Self::shell_state_cache`] to observe newer state snapshots while
  /// the attachment remains active.
  pub shell_state: ShellState,
  /// Server-negotiated attachment liveness settings.
  pub liveness: AttachmentLiveness,
  shell_state_cache: ShellStateCache,
}

impl AttachedSession {
  /// Returns a clone of this attachment's silent, thread-safe shell-state
  /// cache.
  ///
  /// The standard interactive attachment updates this cache from
  /// `shell_state_changed` messages without writing metadata to the terminal.
  #[must_use]
  pub fn shell_state_cache(&self) -> ShellStateCache {
    self.shell_state_cache.clone()
  }
}

/// A latest-value cache of complete shell-awareness state snapshots.
///
/// A cache is initialized from the `attached` snapshot and accepts only
/// strictly newer daemon revisions. It is deliberately independent of raw
/// output sequence tracking: `observed_sequence` helps a renderer correlate
/// state with output, but never changes reconnect/resume behavior.
#[derive(Debug, Clone)]
pub struct ShellStateCache {
  state: Arc<RwLock<ShellState>>,
}

impl ShellStateCache {
  /// Creates a cache initialized with an attachment or one-shot state
  /// snapshot.
  #[must_use]
  pub fn new(initial_state: ShellState) -> Self {
    Self {
      state: Arc::new(RwLock::new(initial_state)),
    }
  }

  /// Returns the latest accepted complete shell-awareness snapshot.
  #[must_use]
  pub fn snapshot(&self) -> ShellState {
    match self.state.read() {
      Ok(state) => state.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  /// Replaces the cached snapshot only when it has a newer daemon revision.
  ///
  /// Returns whether the cache changed. Equal revisions are deliberately
  /// ignored so a delayed event cannot regress per-attachment state.
  #[must_use]
  pub fn apply_if_newer(&self, state: ShellState) -> bool {
    let mut cached_state = match self.state.write() {
      Ok(state) => state,
      Err(poisoned) => poisoned.into_inner(),
    };
    if state.revision <= cached_state.revision {
      return false;
    }

    *cached_state = state;
    true
  }
}

/// A current shell-awareness snapshot retrieved without an attachment.
#[derive(Debug, Clone)]
pub struct SessionShellState {
  pub session: SessionInfo,
  pub shell_state: ShellState,
}

/// Portable attachment liveness settings negotiated during the handshake.
///
/// A client should send a [`ClientMessage::Heartbeat`] at least this often and
/// regard an attachment as disconnected when no server message arrives within
/// [`Self::peer_timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentLiveness {
  pub heartbeat_interval: Duration,
  pub peer_timeout: Duration,
}

/// Metadata returned by a successful `ctmux` handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeInfo {
  pub server_version: String,
  pub protocol_version: u16,
  pub build: Option<ctl_core::component::ComponentBuildInfo>,
  pub attachment_liveness: AttachmentLiveness,
}

/// Optional automatic lease recovery for an interactive attachment.
///
/// This is a fallback after a reconnect token expires and the client must open
/// a new attachment while a stale attachment still owns a lease. The client
/// retries only leases it does not own; it never asks the daemon to displace
/// another logical attachment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InteractiveAttachOptions {
  pub reacquire_input_lease: bool,
  pub reacquire_layout_lease: bool,
  /// Apply the current local terminal size once after a later layout lease
  /// acquisition. The initial attach never resizes without initial layout
  /// ownership.
  pub resize_after_layout_reacquire: bool,
}

/// Why an interactive attachment stopped reading or writing its transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachExitReason {
  Detached,
  ConnectionClosed,
  SessionEnded { exit_code: Option<u32> },
}

/// The final stream position observed by an interactive attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachExit {
  pub reason: AttachExitReason,
  /// The last raw sequence the presentation layer explicitly acknowledged.
  ///
  /// Reconnect using this value, not [`Self::received_sequence`]. It is kept
  /// under the historical field name for existing CLI callers. `None` means a
  /// checkpoint is queued but not yet renderer-acknowledged, so reconnect must
  /// omit `resume_from` and request a new checkpoint.
  pub next_sequence: Option<u64>,
  /// The last raw sequence accepted from the daemon, which can be ahead of
  /// `next_sequence` while a renderer has queued but not yet applied output.
  pub received_sequence: u64,
}

/// Configuration for a renderer-neutral attachment controller.
///
/// The controller owns the attachment transport, heartbeats, server-message
/// decoding, and capability state. A presentation layer owns the returned
/// [`AttachmentControl`] and [`AttachmentEvents`], so it can render raw bytes
/// without taking responsibility for protocol liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentControllerOptions {
  /// Whether the presentation layer can faithfully render the attached PTY
  /// grid before it receives a checkpoint.
  ///
  /// Set this to `false` for a presenter that cannot recreate the daemon's
  /// current grid. Its reconnect cursor remains absent until it applies a
  /// compatible checkpoint.
  pub renderer_starts_compatible: bool,
  /// Retry an unowned input lease at each heartbeat. This never displaces
  /// another attachment.
  pub reacquire_input_lease: bool,
  /// Retry an unowned layout lease at each heartbeat. This never displaces
  /// another attachment.
  pub reacquire_layout_lease: bool,
  /// Apply this explicitly chosen size once after a later successful layout
  /// lease reacquisition. It is ignored when this attachment already owned
  /// layout at startup.
  pub resize_after_layout_reacquire: Option<TerminalSize>,
  /// Maximum locally queued presentation commands before callers backpressure.
  pub command_queue_capacity: usize,
  /// Maximum ordered daemon events buffered for the presentation layer.
  ///
  /// Consumers must continuously drain this queue. The controller deliberately
  /// applies backpressure instead of dropping canonical raw output. This also
  /// bounds the controller's unacknowledged-event ledger, so a consumer that
  /// drains events but never acknowledges them cannot grow memory without
  /// limit.
  pub event_queue_capacity: usize,
}

impl Default for AttachmentControllerOptions {
  fn default() -> Self {
    Self {
      renderer_starts_compatible: true,
      reacquire_input_lease: false,
      reacquire_layout_lease: false,
      resize_after_layout_reacquire: None,
      command_queue_capacity: 64,
      event_queue_capacity: 128,
    }
  }
}

/// An ordered event delivered by an [`AttachmentController`].
///
/// `output`, `checkpoint`, and `pty_geometry_changed` are deliberately kept
/// separate. A renderer must reset or recreate its terminal model for a
/// checkpoint before accepting subsequent raw output; a geometry change affects
/// the PTY/parser grid but never a client viewport or ownership lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentEvent {
  Checkpoint {
    checkpoint: TerminalCheckpoint,
    history: TerminalHistorySnapshot,
    history_manifest: Option<TerminalHistoryManifest>,
    history_gap: bool,
  },
  /// A complete history transfer at an earlier live checkpoint. A presenter
  /// must catch its history model up before publishing it, never restore this
  /// checkpoint directly into the active screen.
  HistorySynced {
    snapshot_id: String,
    checkpoint: TerminalCheckpoint,
    history: TerminalHistorySnapshot,
    rows: Vec<TerminalHistoryRow>,
    scrollback_limit: u64,
    history_gap: bool,
  },
  Output {
    sequence_start: u64,
    sequence_end: u64,
    data: Vec<u8>,
  },
  PtyGeometryChanged {
    terminal_size: TerminalSize,
    observed_sequence: u64,
  },
  LeaseStatus {
    lease: LeaseKind,
    status: LeaseStatus,
  },
  ShellStateChanged {
    state: ShellState,
  },
  HeartbeatAck {
    nonce: u64,
  },
  /// A non-fatal daemon rejection of an input or layout action.
  ServerError {
    code: ErrorCode,
    message: String,
  },
  SessionEnded {
    session_id: String,
    exit_code: Option<u32>,
  },
  Exited {
    exit: AttachExit,
  },
}

/// Current attachment-local lease state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLeases {
  pub input: LeaseStatus,
  pub layout: LeaseStatus,
}

/// A cloneable, thread-safe state cache maintained by an attachment controller.
///
/// Its `resume_sequence` is the only value a reconnecting caller should use as
/// `AttachRequest::resume_from`. It advances only when a presentation layer
/// acknowledges a checkpoint or contiguous raw output, never merely because
/// bytes arrived from the daemon or because advisory shell metadata changed.
#[derive(Debug, Clone)]
pub struct AttachmentState {
  received_sequence: Arc<AtomicU64>,
  resume_sequence: Arc<RwLock<Option<u64>>>,
  leases: Arc<RwLock<AttachmentLeases>>,
  terminal_size: Arc<RwLock<TerminalSize>>,
  shell_state_cache: ShellStateCache,
}

impl AttachmentState {
  fn from_attached(attached: &AttachedSession) -> Self {
    let terminal_size = attached.checkpoint.as_ref().map_or_else(
      || attached.session.terminal_size.clone(),
      |checkpoint| checkpoint.terminal_size.clone(),
    );
    Self {
      received_sequence: Arc::new(AtomicU64::new(attached.replay_from)),
      resume_sequence: Arc::new(RwLock::new(
        attached
          .checkpoint
          .is_none()
          .then_some(attached.replay_from),
      )),
      leases: Arc::new(RwLock::new(AttachmentLeases {
        input: attached.input_lease.clone(),
        layout: attached.layout_lease.clone(),
      })),
      terminal_size: Arc::new(RwLock::new(terminal_size)),
      shell_state_cache: attached.shell_state_cache(),
    }
  }

  /// Returns the raw byte sequence most recently accepted from the daemon.
  ///
  /// This can be ahead of the presentation layer and is never safe as a
  /// reconnect cursor on its own.
  #[must_use]
  pub fn received_sequence(&self) -> u64 {
    self.received_sequence.load(Ordering::Acquire)
  }

  /// Returns the raw byte sequence the presentation layer has applied.
  ///
  /// Use this value for `AttachRequest::resume_from` after a disconnect.
  #[must_use]
  pub fn resume_sequence(&self) -> Option<u64> {
    match self.resume_sequence.read() {
      Ok(resume_sequence) => *resume_sequence,
      Err(poisoned) => *poisoned.into_inner(),
    }
  }

  /// Returns [`Self::resume_sequence`].
  ///
  /// This compatibility spelling intentionally means the safe, applied cursor,
  /// not the newest bytes received from the daemon.
  #[must_use]
  pub fn next_sequence(&self) -> Option<u64> {
    self.resume_sequence()
  }

  /// Returns current input and layout statuses as observed by this attachment.
  #[must_use]
  pub fn leases(&self) -> AttachmentLeases {
    match self.leases.read() {
      Ok(leases) => leases.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  /// Returns the last authoritative PTY geometry received by this attachment.
  #[must_use]
  pub fn terminal_size(&self) -> TerminalSize {
    match self.terminal_size.read() {
      Ok(terminal_size) => terminal_size.clone(),
      Err(poisoned) => poisoned.into_inner().clone(),
    }
  }

  /// Returns the shared current shell-awareness cache.
  #[must_use]
  pub fn shell_state_cache(&self) -> ShellStateCache {
    self.shell_state_cache.clone()
  }

  fn set_lease(&self, lease: LeaseKind, status: LeaseStatus) {
    let mut leases = match self.leases.write() {
      Ok(leases) => leases,
      Err(poisoned) => poisoned.into_inner(),
    };
    match lease {
      LeaseKind::Input => leases.input = status,
      LeaseKind::Layout => leases.layout = status,
    }
  }

  fn mark_lease_not_owned(&self, lease: LeaseKind) {
    let mut leases = match self.leases.write() {
      Ok(leases) => leases,
      Err(poisoned) => poisoned.into_inner(),
    };
    match lease {
      LeaseKind::Input => leases.input.owned_by_client = false,
      LeaseKind::Layout => leases.layout.owned_by_client = false,
    }
  }

  fn lease_status(&self, lease: LeaseKind) -> LeaseStatus {
    let leases = self.leases();
    match lease {
      LeaseKind::Input => leases.input,
      LeaseKind::Layout => leases.layout,
    }
  }

  fn set_terminal_size(&self, terminal_size: TerminalSize) {
    let mut cached_size = match self.terminal_size.write() {
      Ok(terminal_size) => terminal_size,
      Err(poisoned) => poisoned.into_inner(),
    };
    *cached_size = terminal_size;
  }

  fn set_received_sequence(&self, received_sequence: u64) {
    self
      .received_sequence
      .store(received_sequence, Ordering::Release);
  }

  fn set_resume_sequence(&self, resume_sequence: Option<u64>) {
    let mut cached_sequence = match self.resume_sequence.write() {
      Ok(resume_sequence) => resume_sequence,
      Err(poisoned) => poisoned.into_inner(),
    };
    *cached_sequence = resume_sequence;
  }
}

/// Commands a presentation layer may submit to an active attachment.
///
/// Heartbeats are intentionally absent: they are generated by the controller.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachmentCommand {
  Input { data: Vec<u8> },
  Resize { terminal_size: TerminalSize },
  AcquireLease { lease: LeaseKind },
  ReleaseLease { lease: LeaseKind },
  RequestCheckpoint,
  Detach,
}

/// Confirms that a renderer has applied one ordered presentation event.
///
/// The controller uses these local messages both to determine the safe
/// reconnect cursor and to publish coalesced delivery progress to ctmuxd. A
/// renderer never writes protocol frames itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationAck {
  Output { sequence_end: u64 },
  Checkpoint { sequence: u64 },
  CheckpointIncompatible { sequence: u64 },
  Geometry { observed_sequence: u64 },
  GeometryIncompatible { observed_sequence: u64 },
}

struct PresentationAcknowledgement {
  acknowledgement: PresentationAck,
  completion: oneshot::Sender<Result<(), AttachmentAcknowledgementError>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPresentation {
  Output { sequence_end: u64 },
  Checkpoint { sequence: u64 },
  Geometry { observed_sequence: u64 },
}

/// Failure to queue a presentation command locally.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentCommandError {
  #[error("this attachment does not own the input lease")]
  InputLeaseRequired,
  #[error("this attachment does not own the PTY layout lease")]
  LayoutLeaseRequired,
  #[error("attachment controller is no longer running")]
  Closed,
}

/// Failure to apply a renderer acknowledgement in the attachment controller.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentAcknowledgementError {
  #[error("attachment controller is no longer running")]
  Closed,
  #[error("unexpected presentation acknowledgement {actual}; expected {expected}")]
  Rejected { expected: String, actual: String },
}

/// Cloneable command endpoint for an [`AttachmentController`].
///
/// A successful command method only means the request entered the local
/// ordered queue. The daemon remains authoritative; a race with lease loss is
/// reported later as [`AttachmentEvent::ServerError`]. Renderer
/// acknowledgement methods return only after the controller accepts the next
/// ordered presentation event and updates its renderer-applied resume state.
#[derive(Debug, Clone)]
pub struct AttachmentControl {
  commands: mpsc::Sender<AttachmentCommand>,
  acknowledgements: mpsc::Sender<PresentationAcknowledgement>,
  state: AttachmentState,
}

impl AttachmentControl {
  /// Returns the state cache maintained by the controller.
  #[must_use]
  pub fn state(&self) -> AttachmentState {
    self.state.clone()
  }

  /// Queues raw PTY input when this attachment currently owns input.
  ///
  /// # Errors
  ///
  /// Returns an error when input is not currently owned or the controller has
  /// already stopped.
  pub async fn input(&self, data: Vec<u8>) -> Result<(), AttachmentCommandError> {
    if !self.state.lease_status(LeaseKind::Input).owned_by_client {
      return Err(AttachmentCommandError::InputLeaseRequired);
    }
    self.send(AttachmentCommand::Input { data }).await
  }

  /// Queues an explicit PTY resize when this attachment currently owns layout.
  ///
  /// # Errors
  ///
  /// Returns an error when layout is not currently owned or the controller has
  /// already stopped.
  pub async fn resize(&self, terminal_size: TerminalSize) -> Result<(), AttachmentCommandError> {
    if !self.state.lease_status(LeaseKind::Layout).owned_by_client {
      return Err(AttachmentCommandError::LayoutLeaseRequired);
    }
    self.send(AttachmentCommand::Resize { terminal_size }).await
  }

  /// Asks the daemon to acquire an unheld input or layout lease.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn acquire_lease(&self, lease: LeaseKind) -> Result<(), AttachmentCommandError> {
    self.send(AttachmentCommand::AcquireLease { lease }).await
  }

  /// Releases this attachment's input or layout lease.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn release_lease(&self, lease: LeaseKind) -> Result<(), AttachmentCommandError> {
    self.send(AttachmentCommand::ReleaseLease { lease }).await
  }

  /// Gracefully detaches without ending the persistent remote session.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn detach(&self) -> Result<(), AttachmentCommandError> {
    self.send(AttachmentCommand::Detach).await
  }

  /// Requests an authoritative recovery boundary without changing leases.
  ///
  /// # Errors
  /// Returns an error when this attachment has already closed.
  pub async fn request_checkpoint(&self) -> Result<(), AttachmentCommandError> {
    self.send(AttachmentCommand::RequestCheckpoint).await
  }

  /// Acknowledges that the renderer applied an `output` event through this
  /// exact sequence end.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has stopped or this does not match
  /// the next ordered presentation event.
  pub async fn acknowledge_output(
    &self,
    sequence_end: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledge(PresentationAck::Output { sequence_end })
      .await
  }

  /// Acknowledges that the renderer reset/applied a `checkpoint` event.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has stopped or this does not match
  /// the next ordered presentation event.
  pub async fn acknowledge_checkpoint(
    &self,
    sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledge(PresentationAck::Checkpoint { sequence })
      .await
  }

  /// Records a checkpoint that was displayed but not applied to a compatible
  /// terminal grid.
  ///
  /// Later output acknowledgements remain bookkeeping only; reconnect will
  /// keep omitting `resume_from` until a compatible checkpoint is applied.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has stopped or this does not match
  /// the next ordered presentation event.
  pub async fn acknowledge_checkpoint_incompatible(
    &self,
    sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledge(PresentationAck::CheckpointIncompatible { sequence })
      .await
  }

  /// Acknowledges that the renderer applied an ordered PTY geometry change.
  ///
  /// Geometry does not advance the raw resume cursor, but acknowledgement
  /// prevents a later output acknowledgement from overtaking the resize.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has stopped or this does not match
  /// the next ordered presentation event.
  pub async fn acknowledge_geometry(
    &self,
    observed_sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledge(PresentationAck::Geometry { observed_sequence })
      .await
  }

  /// Records an observed geometry transition that this renderer cannot apply.
  ///
  /// This removes the event from the ordered acknowledgement queue but keeps
  /// the reconnect cursor absent until a later checkpoint is successfully
  /// applied. Use it for a presentation such as a raw terminal that cannot
  /// adopt the daemon's PTY grid without changing its local viewport.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has stopped or this does not match
  /// the next ordered presentation event.
  pub async fn acknowledge_geometry_incompatible(
    &self,
    observed_sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledge(PresentationAck::GeometryIncompatible { observed_sequence })
      .await
  }

  async fn acknowledge(
    &self,
    acknowledgement: PresentationAck,
  ) -> Result<(), AttachmentAcknowledgementError> {
    let (completion, completed) = oneshot::channel();
    self
      .acknowledgements
      .send(PresentationAcknowledgement {
        acknowledgement,
        completion,
      })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)?;
    completed
      .await
      .unwrap_or(Err(AttachmentAcknowledgementError::Closed))
  }

  async fn send(&self, command: AttachmentCommand) -> Result<(), AttachmentCommandError> {
    self
      .commands
      .send(command)
      .await
      .map_err(|_error| AttachmentCommandError::Closed)
  }
}

/// Ordered event receiver for an [`AttachmentController`].
#[derive(Debug)]
pub struct AttachmentEvents {
  receiver: mpsc::Receiver<AttachmentEvent>,
}

impl AttachmentEvents {
  /// Waits for the next ordered attachment event.
  pub async fn recv(&mut self) -> Option<AttachmentEvent> {
    self.receiver.recv().await
  }

  /// Attempts to receive an already-buffered attachment event.
  ///
  /// # Errors
  ///
  /// Returns `Empty` when no event is buffered or `Disconnected` after the
  /// controller closes the event stream.
  pub fn try_recv(&mut self) -> Result<AttachmentEvent, mpsc::error::TryRecvError> {
    self.receiver.try_recv()
  }
}

/// Renderer-neutral attachment driver.
///
/// Construct it after [`begin_attach`], then run it in a task appropriate for
/// the host runtime. The stream remains generic so local IPC and an injected
/// `ctl` byte stream use exactly the same attachment semantics.
pub struct AttachmentController<S> {
  stream: Option<S>,
  state: AttachmentState,
  liveness: AttachmentLiveness,
  options: AttachmentControllerOptions,
  commands: Option<mpsc::Receiver<AttachmentCommand>>,
  acknowledgements: Option<mpsc::Receiver<PresentationAcknowledgement>>,
  events: mpsc::Sender<AttachmentEvent>,
  initial_checkpoint: Option<CheckpointDelivery>,
  history_transfer: Option<history_sync::HistoryTransfer>,
  pending_history: Option<AttachmentEvent>,
  history_started: bool,
  history_requests: Option<watch::Sender<Option<ClientMessage>>>,
  /// A renderer may continue to present raw bytes after declaring a geometry
  /// incompatible, but it cannot resume safely until a checkpoint is applied.
  renderer_requires_checkpoint: bool,
  pending_presentations: VecDeque<PendingPresentation>,
}

struct CheckpointDelivery {
  checkpoint: TerminalCheckpoint,
  history: TerminalHistorySnapshot,
  history_manifest: Option<TerminalHistoryManifest>,
  history_gap: bool,
}

/// Performs the versioned `ctmux` handshake over any bidirectional stream.
///
/// # Errors
///
/// Returns liveness metadata after a successful handshake, or an error when
/// the transport fails, the daemon rejects the request, or its reply is
/// malformed.
pub async fn handshake<S>(
  stream: &mut S,
  identity: &ClientIdentity,
) -> Result<HandshakeInfo, ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: PROTOCOL_VERSION,
      client_name: identity.name.clone(),
      client_version: identity.version.clone(),
    },
  )
  .await?;

  match read_response(stream).await? {
    ServerMessage::HandshakeAccepted {
      protocol_version,
      server_version,
      build,
      heartbeat_interval_ms,
      attachment_liveness_timeout_ms,
      ..
    } if protocol_version == PROTOCOL_VERSION => {
      attachment_liveness(heartbeat_interval_ms, attachment_liveness_timeout_ms).map(
        |attachment_liveness| HandshakeInfo {
          server_version,
          protocol_version,
          build,
          attachment_liveness,
        },
      )
    }
    response => Err(unexpected("handshake_accepted", &response)),
  }
}

/// Sends a single non-attachment request over any supported transport.
///
/// The stream is consumed because one-shot `ctmux` requests complete after the
/// daemon sends their response.
///
/// # Errors
///
/// Returns an error when the handshake, request, or response fails.
pub async fn request<S>(
  mut stream: S,
  identity: &ClientIdentity,
  message: ClientMessage,
) -> Result<ServerMessage, ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  handshake(&mut stream, identity).await?;
  write_frame(&mut stream, &message).await?;
  read_response(&mut stream).await
}

/// Retrieves the current shell-awareness state without attaching to a session.
///
/// The daemon applies its command-line visibility policy to the returned
/// snapshot.
///
/// # Errors
///
/// Returns an error when the handshake, request, or response fails, or when
/// the daemon returns an unexpected response.
pub async fn get_shell_state<S>(
  stream: S,
  identity: &ClientIdentity,
  session: impl Into<String>,
) -> Result<SessionShellState, ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  match request(
    stream,
    identity,
    ClientMessage::GetShellState {
      session: session.into(),
    },
  )
  .await?
  {
    ServerMessage::ShellStateResponse {
      session,
      shell_state,
    } => Ok(SessionShellState {
      session,
      shell_state,
    }),
    response => Err(unexpected("shell_state_response", &response)),
  }
}

/// Opens an attachment over any supported transport.
///
/// The caller retains the stream and passes it to [`attach_interactive`] or a
/// custom renderer after this function returns.
///
/// # Errors
///
/// Returns an error when the handshake or attach request fails, or when the
/// daemon's first attachment message is not `attached`.
pub async fn begin_attach<S>(
  stream: S,
  identity: &ClientIdentity,
  request: AttachRequest,
) -> Result<(S, AttachedSession), ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let message = ClientMessage::AttachSession {
    session: request.session,
    resume_from: request.resume_from,
    terminal_size: request.terminal_size,
    request_input_lease: request.request_input_lease,
    request_layout_lease: request.request_layout_lease,
    request_command_line: request.request_command_line,
    request_running_command: request.request_running_command,
    presentation_window_bytes: request.presentation_window_bytes,
  };
  begin_attachment(stream, identity, message).await
}

/// Rebinds a replacement transport to a recently disconnected attachment.
///
/// The daemon validates the opaque token, supersedes the prior transport, and
/// preserves the logical attachment's existing leases. `resume_from` remains
/// renderer-owned and independently determines raw output replay.
///
/// # Errors
///
/// Returns an error when the handshake fails, the token is invalid or
/// expired, or the daemon's first attachment message is malformed.
pub async fn resume_attach<S>(
  stream: S,
  identity: &ClientIdentity,
  attachment_token: String,
  request: AttachRequest,
) -> Result<(S, AttachedSession), ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let message = ClientMessage::ResumeAttachment {
    session: request.session,
    attachment_token,
    resume_from: request.resume_from,
    terminal_size: request.terminal_size,
    request_command_line: request.request_command_line,
    request_running_command: request.request_running_command,
    presentation_window_bytes: request.presentation_window_bytes,
  };
  begin_attachment(stream, identity, message).await
}

async fn begin_attachment<S>(
  mut stream: S,
  identity: &ClientIdentity,
  message: ClientMessage,
) -> Result<(S, AttachedSession), ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let handshake = handshake(&mut stream, identity).await?;
  write_frame(&mut stream, &message).await?;

  let response = read_response(&mut stream).await?;
  let ServerMessage::Attached {
    attachment_token,
    session,
    replay_from,
    history_gap,
    checkpoint,
    history,
    history_manifest,
    terminal_size_mismatch,
    input_lease,
    layout_lease,
    shell_state,
    ..
  } = response
  else {
    return Err(unexpected("attached", &response));
  };

  Ok((
    stream,
    AttachedSession {
      attachment_token,
      session,
      replay_from,
      history_gap,
      checkpoint,
      history: history.map(|history| *history),
      history_manifest: history_manifest.map(|manifest| *manifest),
      terminal_size_mismatch,
      input_lease,
      layout_lease,
      shell_state_cache: ShellStateCache::new(shell_state.clone()),
      shell_state,
      liveness: handshake.attachment_liveness,
      handshake_info: handshake,
    },
  ))
}

impl<S> AttachmentController<S> {
  /// Creates a renderer-neutral controller for a completed attachment.
  ///
  /// The returned [`AttachmentControl`] can be cloned into a GUI input bridge,
  /// while [`AttachmentEvents`] stays with its renderer. Run the controller in
  /// a task; it remains generic over the selected local or remote transport.
  ///
  /// # Errors
  ///
  /// Returns an error when queue capacities are zero or the initial checkpoint
  /// cannot safely seed a renderer's ordered raw-output stream.
  pub fn new(
    stream: S,
    attached: &AttachedSession,
    options: AttachmentControllerOptions,
  ) -> Result<(Self, AttachmentControl, AttachmentEvents), ClientError> {
    if options.command_queue_capacity == 0 || options.event_queue_capacity == 0 {
      return Err(ClientError::InvalidAttachmentQueueCapacity {
        command_queue_capacity: options.command_queue_capacity,
        event_queue_capacity: options.event_queue_capacity,
      });
    }
    match (attached.checkpoint.as_ref(), attached.history.as_ref()) {
      (Some(checkpoint), Some(history)) => {
        validate_checkpoint_bundle(checkpoint, history)?;
        if checkpoint.sequence != attached.replay_from {
          return Err(ClientError::InvalidInitialCheckpointSequence {
            checkpoint_sequence: checkpoint.sequence,
            replay_from: attached.replay_from,
          });
        }
      }
      (None, None) => {}
      _ => return Err(ClientError::CheckpointHistoryPresenceMismatch),
    }

    let state = AttachmentState::from_attached(attached);
    let (command_sender, commands) = mpsc::channel(options.command_queue_capacity);
    let (acknowledgement_sender, acknowledgements) = mpsc::channel(options.event_queue_capacity);
    let (events, event_receiver) = mpsc::channel(options.event_queue_capacity);
    let initial_checkpoint = attached
      .checkpoint
      .as_ref()
      .zip(attached.history.as_ref())
      .map(|(checkpoint, history)| CheckpointDelivery {
        checkpoint: checkpoint.clone(),
        history: history.clone(),
        history_manifest: attached.history_manifest.clone(),
        history_gap: attached.history_gap,
      });
    let history_transfer = attached
      .history_manifest
      .clone()
      .map(|manifest| {
        history_sync::HistoryTransfer::new(
          manifest,
          attached
            .checkpoint
            .clone()
            .ok_or(ClientError::CheckpointHistoryPresenceMismatch)?,
          attached
            .history
            .clone()
            .ok_or(ClientError::CheckpointHistoryPresenceMismatch)?,
          attached.history_gap,
        )
      })
      .transpose()?;
    let renderer_requires_checkpoint =
      !options.renderer_starts_compatible || attached.checkpoint.is_some();
    if renderer_requires_checkpoint {
      state.set_resume_sequence(None);
    }
    let control = AttachmentControl {
      commands: command_sender,
      acknowledgements: acknowledgement_sender,
      state: state.clone(),
    };

    Ok((
      Self {
        stream: Some(stream),
        state,
        liveness: attached.liveness,
        options,
        commands: Some(commands),
        acknowledgements: Some(acknowledgements),
        events,
        initial_checkpoint,
        history_transfer,
        pending_history: None,
        history_started: false,
        history_requests: None,
        renderer_requires_checkpoint,
        pending_presentations: VecDeque::new(),
      },
      control,
      AttachmentEvents {
        receiver: event_receiver,
      },
    ))
  }

  /// Returns the cloneable cache that survives a transport disconnect.
  ///
  /// Use [`AttachmentState::resume_sequence`] as the next
  /// [`AttachRequest::resume_from`] value after selecting a new transport and
  /// calling [`begin_attach`] again.
  #[must_use]
  pub fn state(&self) -> AttachmentState {
    self.state.clone()
  }

  /// Runs the controller until detach, session exit, connection loss, or a
  /// fatal protocol error.
  ///
  /// This method does not render terminal bytes, read local input, or choose a
  /// viewport. It serializes outgoing commands, sends heartbeats, validates
  /// contiguous raw sequence ranges, and forwards only ordered presentation
  /// events.
  ///
  /// # Errors
  ///
  /// Returns an error for malformed or unsupported protocol state, a fatal
  /// server error, or a non-I/O transport/codec failure. Ordinary EOF and I/O
  /// disconnects return [`AttachExitReason::ConnectionClosed`].
  pub async fn run(mut self) -> Result<AttachExit, ClientError>
  where
    S: AsyncRead + AsyncWrite + Unpin,
  {
    let Some(stream) = self.stream.take() else {
      return Err(ClientError::AttachmentControllerAlreadyRun);
    };
    let Some(commands) = self.commands.take() else {
      return Err(ClientError::AttachmentControllerAlreadyRun);
    };
    let Some(acknowledgements) = self.acknowledgements.take() else {
      return Err(ClientError::AttachmentControllerAlreadyRun);
    };
    let (reader, writer) = tokio::io::split(stream);
    let (incoming_sender, incoming_receiver) = mpsc::channel(self.options.event_queue_capacity);
    let (writer_status_sender, writer_status_receiver) = mpsc::unbounded_channel();
    let (presentation_progress_sender, presentation_progress_receiver) = watch::channel(None);
    let (history_request_sender, history_request_receiver) = watch::channel(None);
    self.history_requests = Some(history_request_sender);
    let peer_activity = Arc::new(Mutex::new(Instant::now()));
    let reader = read_server_messages(reader, incoming_sender, Arc::clone(&peer_activity));
    let peer_silence = wait_for_peer_silence(peer_activity, self.liveness.peer_timeout);
    let drain_timeout = self.liveness.peer_timeout;
    let writer = drive_attachment_writer(
      writer,
      self.state.clone(),
      self.liveness,
      self.options.clone(),
      AttachmentWriterChannels {
        commands,
        presentation_progress: presentation_progress_receiver,
        history_requests: history_request_receiver,
        statuses: writer_status_sender,
      },
    );
    // Keep liveness outside all I/O and presentation waits. A blackholed SSH
    // pipe may block a write forever, including the heartbeat meant to detect
    // that failure. Canceling these futures closes only this attachment.
    let result = {
      let driver = self.drive(
        incoming_receiver,
        acknowledgements,
        presentation_progress_sender,
        writer_status_receiver,
      );
      tokio::pin!(reader);
      tokio::pin!(writer);
      tokio::pin!(driver);
      tokio::pin!(peer_silence);
      let mut reader_finished = false;
      let mut writer_finished = false;
      let writer_drain = tokio::time::sleep(drain_timeout);
      tokio::pin!(writer_drain);
      loop {
        tokio::select! {
          biased;
          result = &mut driver => break Some(result),
          () = &mut reader, if !reader_finished => reader_finished = true,
          () = &mut writer, if !writer_finished => {
            writer_finished = true;
            // A failed final acknowledgement does not erase already-sent
            // output or SessionEnded. Keep reading, but bound the drain even
            // if a one-way peer continues sending without accepting writes.
            writer_drain.as_mut().reset(Instant::now() + drain_timeout);
          }
          () = &mut peer_silence => break None,
          () = &mut writer_drain, if writer_finished => break None,
        }
      }
    };
    match result {
      Some(result) => result,
      None => Ok(self.finish(AttachExitReason::ConnectionClosed)),
    }
  }

  async fn drive(
    &mut self,
    mut incoming: mpsc::Receiver<IncomingServerMessage>,
    mut acknowledgements: mpsc::Receiver<PresentationAcknowledgement>,
    presentation_progress: watch::Sender<Option<u64>>,
    mut writer_statuses: mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<AttachExit, ClientError> {
    if let Some(CheckpointDelivery {
      checkpoint,
      history,
      history_manifest,
      history_gap,
    }) = self.initial_checkpoint.take()
    {
      let queued = self.enqueue_presentation(PendingPresentation::Checkpoint {
        sequence: checkpoint.sequence,
      });
      debug_assert!(
        queued,
        "a non-zero event queue accepts the initial checkpoint"
      );
      match self
        .emit_event(
          AttachmentEvent::Checkpoint {
            checkpoint,
            history,
            history_manifest,
            history_gap,
          },
          &mut writer_statuses,
        )
        .await?
      {
        ControllerAction::Continue => {}
        ControllerAction::Exit { reason, .. } => return Ok(self.finish(reason)),
      }
    }

    let mut acknowledgements_open = true;

    loop {
      let presentation_capacity_available =
        self.pending_presentations.len() < self.options.event_queue_capacity;
      let event_sender = self.events.clone();
      let event_capacity_available = event_sender.capacity() > 0;
      let history_pending = self.pending_history.is_some();
      tokio::select! {
        error = writer_failure(&mut writer_statuses) => return Err(error),
        acknowledgement = acknowledgements.recv(), if acknowledgements_open => {
          match acknowledgement {
            Some(acknowledgement) => {
              if let Some(applied_sequence) =
                self.accept_presentation_acknowledgement_request(acknowledgement)?
              {
                presentation_progress.send_replace(Some(applied_sequence));
                if !self.history_started && self.history_transfer.as_ref().is_some_and(|transfer| transfer.sequence() <= applied_sequence) {
                  self.history_started = true;
                  self.advance_history_transfer()?;
                }
              }
            }
            None => acknowledgements_open = false,
          }
        }
        incoming_message = incoming.recv(), if presentation_capacity_available && event_capacity_available && !history_pending => {
          match incoming_message {
            Some(IncomingServerMessage::Message(message)) => {
              match self.process_server_message(*message, &mut writer_statuses).await? {
                ControllerAction::Continue => {}
                ControllerAction::Exit { reason, .. } => {
                  return Ok(self.finish(reason));
                }
              }
            }
            Some(IncomingServerMessage::ConnectionClosed) | None => {
              return Ok(self.finish(AttachExitReason::ConnectionClosed));
            }
            Some(IncomingServerMessage::Fatal(error)) => return Err(error),
          }
        }
        // Wait for channel capacity without blocking acknowledgement handling.
        // History completion never participates in the presentation ledger.
        permit = event_sender.reserve(), if history_pending || !event_capacity_available => {
          let Ok(permit) = permit else { return Ok(self.finish(AttachExitReason::Detached)); };
          if let Some(history) = self.pending_history.take() {
            permit.send(history);
          }
        }
      }
    }
  }

  async fn process_server_message(
    &mut self,
    message: ServerMessage,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    match message {
      ServerMessage::Output {
        sequence_start,
        sequence_end,
        data,
      } => {
        self
          .process_output(sequence_start, sequence_end, data, writer_statuses)
          .await
      }
      ServerMessage::Checkpoint {
        checkpoint,
        history,
        history_manifest,
        history_gap,
      } => {
        self
          .process_checkpoint(
            checkpoint,
            *history,
            history_manifest.map(|manifest| *manifest),
            history_gap,
            writer_statuses,
          )
          .await
      }
      ServerMessage::HistoryPage {
        snapshot_id,
        offset,
        data,
        next_offset,
      } => self.process_history_page(&snapshot_id, offset, &data, next_offset),
      ServerMessage::HistorySnapshotExpired { snapshot_id } => {
        self.expire_history_transfer(&snapshot_id);
        Ok(ControllerAction::Continue)
      }
      ServerMessage::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        self
          .accept_geometry_change(terminal_size, observed_sequence, writer_statuses)
          .await
      }
      ServerMessage::LeaseStatus { lease, status } => {
        self
          .process_lease_status(lease, status, writer_statuses)
          .await
      }
      ServerMessage::ShellStateChanged { state } => {
        self.process_shell_state(state, writer_statuses).await
      }
      ServerMessage::HeartbeatAck { nonce } => {
        self
          .emit_event(AttachmentEvent::HeartbeatAck { nonce }, writer_statuses)
          .await
      }
      ServerMessage::Detached => Ok(ControllerAction::Exit {
        reason: AttachExitReason::Detached,
      }),
      ServerMessage::SessionEnded {
        session_id,
        exit_code,
      } => {
        self
          .process_session_ended(session_id, exit_code, writer_statuses)
          .await
      }
      ServerMessage::Error { code, message } => {
        self
          .process_server_error(code, message, writer_statuses)
          .await
      }
      response => Err(unexpected(
        "output, checkpoint, pty_geometry_changed, shell_state_changed, lease_status, heartbeat_ack, detached, or session_ended",
        &response,
      )),
    }
  }

  async fn process_output(
    &mut self,
    sequence_start: u64,
    sequence_end: u64,
    data: Vec<u8>,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    self.accept_output(sequence_start, sequence_end, &data)?;
    let queued = self.enqueue_presentation(PendingPresentation::Output { sequence_end });
    debug_assert!(
      queued,
      "presentation capacity is checked before receiving output"
    );
    self
      .emit_event(
        AttachmentEvent::Output {
          sequence_start,
          sequence_end,
          data,
        },
        writer_statuses,
      )
      .await
  }

  async fn process_checkpoint(
    &mut self,
    checkpoint: TerminalCheckpoint,
    history: TerminalHistorySnapshot,
    history_manifest: Option<TerminalHistoryManifest>,
    history_gap: bool,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    self.accept_checkpoint(&checkpoint, &history)?;
    self.history_transfer = history_manifest
      .clone()
      .map(|manifest| {
        history_sync::HistoryTransfer::new(
          manifest,
          checkpoint.clone(),
          history.clone(),
          history_gap,
        )
      })
      .transpose()?;
    self.history_started = false;
    self.pending_history = None;
    self.queue_history_request(None);
    let queued = self.enqueue_presentation(PendingPresentation::Checkpoint {
      sequence: checkpoint.sequence,
    });
    debug_assert!(
      queued,
      "presentation capacity is checked before receiving checkpoints"
    );
    self
      .emit_event(
        AttachmentEvent::Checkpoint {
          checkpoint,
          history,
          history_manifest,
          history_gap,
        },
        writer_statuses,
      )
      .await
  }

  fn queue_history_request(&self, message: Option<ClientMessage>) {
    if let Some(requests) = &self.history_requests {
      requests.send_replace(message);
    }
  }

  fn advance_history_transfer(&mut self) -> Result<(), ClientError> {
    if self
      .history_transfer
      .as_ref()
      .is_some_and(history_sync::HistoryTransfer::is_complete)
    {
      let transfer = self
        .history_transfer
        .take()
        .expect("complete history transfer exists");
      self.queue_history_request(None);
      self.pending_history = Some(transfer.finish()?);
    } else {
      self.queue_history_request(
        self
          .history_transfer
          .as_ref()
          .map(history_sync::HistoryTransfer::request),
      );
    }
    Ok(())
  }

  fn process_history_page(
    &mut self,
    snapshot_id: &str,
    offset: u64,
    data: &[u8],
    next_offset: Option<u64>,
  ) -> Result<ControllerAction, ClientError> {
    let Some(transfer) = self
      .history_transfer
      .as_mut()
      .filter(|transfer| transfer.snapshot_id() == snapshot_id)
    else {
      // A checkpoint may replace a transfer while its previous request is in
      // flight. Those pages never become part of the new projection.
      return Ok(ControllerAction::Continue);
    };
    transfer.accept_page(offset, data, next_offset)?;
    self.advance_history_transfer()?;
    Ok(ControllerAction::Continue)
  }

  fn expire_history_transfer(&mut self, snapshot_id: &str) {
    if self
      .history_transfer
      .as_ref()
      .is_some_and(|transfer| transfer.snapshot_id() == snapshot_id)
    {
      self.history_transfer = None;
      self.history_started = false;
      self.pending_history = None;
      self.queue_history_request(Some(ClientMessage::RequestCheckpoint));
    }
  }

  async fn process_lease_status(
    &mut self,
    lease: LeaseKind,
    status: LeaseStatus,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    self.state.set_lease(lease, status.clone());
    self
      .emit_event(
        AttachmentEvent::LeaseStatus { lease, status },
        writer_statuses,
      )
      .await
  }

  async fn process_shell_state(
    &mut self,
    state: ShellState,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    if self.state.shell_state_cache.apply_if_newer(state.clone()) {
      self
        .emit_event(
          AttachmentEvent::ShellStateChanged { state },
          writer_statuses,
        )
        .await
    } else {
      Ok(ControllerAction::Continue)
    }
  }

  async fn process_session_ended(
    &mut self,
    session_id: String,
    exit_code: Option<u32>,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    match self
      .emit_event(
        AttachmentEvent::SessionEnded {
          session_id,
          exit_code,
        },
        writer_statuses,
      )
      .await?
    {
      ControllerAction::Continue => Ok(ControllerAction::Exit {
        reason: AttachExitReason::SessionEnded { exit_code },
      }),
      exit @ ControllerAction::Exit { .. } => Ok(exit),
    }
  }

  async fn process_server_error(
    &mut self,
    code: ErrorCode,
    message: String,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    let lease = match code {
      ErrorCode::InputLeaseRequired => LeaseKind::Input,
      ErrorCode::LayoutLeaseRequired => LeaseKind::Layout,
      _ => return Err(ClientError::Server { code, message }),
    };
    self.state.mark_lease_not_owned(lease);
    self
      .emit_event(
        AttachmentEvent::ServerError { code, message },
        writer_statuses,
      )
      .await
  }

  fn accept_output(
    &self,
    sequence_start: u64,
    sequence_end: u64,
    data: &[u8],
  ) -> Result<(), ClientError> {
    let expected_sequence = self.state.received_sequence();
    let frame_end = u64::try_from(data.len())
      .ok()
      .and_then(|length| sequence_start.checked_add(length));
    if sequence_start != expected_sequence || frame_end != Some(sequence_end) {
      return Err(ClientError::InvalidOutputSequence {
        expected_sequence,
        sequence_start,
        sequence_end,
        data_len: data.len(),
      });
    }
    self.state.set_received_sequence(sequence_end);
    Ok(())
  }

  fn accept_checkpoint(
    &mut self,
    checkpoint: &TerminalCheckpoint,
    history: &TerminalHistorySnapshot,
  ) -> Result<(), ClientError> {
    validate_checkpoint_bundle(checkpoint, history)?;
    let previous_sequence = self.state.received_sequence();
    if checkpoint.sequence < previous_sequence {
      return Err(ClientError::StaleCheckpoint {
        checkpoint_sequence: checkpoint.sequence,
        previous_sequence,
      });
    }
    self.state.set_received_sequence(checkpoint.sequence);
    self.state.set_resume_sequence(None);
    self.renderer_requires_checkpoint = true;
    self
      .state
      .set_terminal_size(checkpoint.terminal_size.clone());
    Ok(())
  }

  async fn accept_geometry_change(
    &mut self,
    terminal_size: TerminalSize,
    observed_sequence: u64,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    if observed_sequence < self.state.received_sequence() {
      return Ok(ControllerAction::Continue);
    }
    self.history_transfer = None;
    self.history_started = false;
    self.pending_history = None;
    self.queue_history_request(None);
    let expected_sequence = self.state.received_sequence();
    if observed_sequence != expected_sequence {
      return Err(ClientError::GeometryAheadOfOutput {
        expected_sequence,
        observed_sequence,
      });
    }

    self.state.set_terminal_size(terminal_size.clone());
    let queued = self.enqueue_presentation(PendingPresentation::Geometry { observed_sequence });
    debug_assert!(
      queued,
      "presentation capacity is checked before receiving geometry"
    );
    self
      .emit_event(
        AttachmentEvent::PtyGeometryChanged {
          terminal_size,
          observed_sequence,
        },
        writer_statuses,
      )
      .await
  }

  fn enqueue_presentation(&mut self, pending: PendingPresentation) -> bool {
    if self.pending_presentations.len() >= self.options.event_queue_capacity {
      return false;
    }
    self.pending_presentations.push_back(pending);
    true
  }

  async fn emit_event(
    &mut self,
    event: AttachmentEvent,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    let send = self.events.send(event);
    tokio::pin!(send);
    tokio::select! {
      result = &mut send => {
        if result.is_ok() {
          Ok(ControllerAction::Continue)
        } else {
          Ok(ControllerAction::Exit {
            reason: AttachExitReason::Detached,
          })
        }
      }
      error = writer_failure(writer_statuses) => Err(error),
    }
  }

  fn accept_presentation_acknowledgement(
    &mut self,
    acknowledgement: PresentationAck,
  ) -> Result<Option<u64>, ClientError> {
    let pending = self.pending_presentations.front().copied().ok_or_else(|| {
      ClientError::UnexpectedPresentationAcknowledgement {
        expected: "no pending presentation event".into(),
        actual: presentation_acknowledgement_name(acknowledgement).into(),
      }
    })?;
    let (
      acknowledged_sequence,
      applied_checkpoint,
      incompatible_checkpoint,
      incompatible_geometry,
      delivery_progress,
    ) = match (pending, acknowledgement) {
      (
        PendingPresentation::Output {
          sequence_end: expected_sequence_end,
        },
        PresentationAck::Output { sequence_end },
      ) if sequence_end == expected_sequence_end => {
        (Some(sequence_end), false, false, false, Some(sequence_end))
      }
      (
        PendingPresentation::Checkpoint {
          sequence: expected_sequence,
        },
        PresentationAck::Checkpoint { sequence },
      ) if sequence == expected_sequence => (Some(sequence), true, false, false, Some(sequence)),
      (
        PendingPresentation::Checkpoint {
          sequence: expected_sequence,
        },
        PresentationAck::CheckpointIncompatible { sequence },
      ) if sequence == expected_sequence => (None, false, true, false, Some(sequence)),
      (
        PendingPresentation::Geometry {
          observed_sequence: expected_sequence,
        },
        PresentationAck::Geometry { observed_sequence },
      ) if observed_sequence == expected_sequence => {
        (self.state.resume_sequence(), false, false, false, None)
      }
      (
        PendingPresentation::Geometry {
          observed_sequence: expected_sequence,
        },
        PresentationAck::GeometryIncompatible { observed_sequence },
      ) if observed_sequence == expected_sequence => (None, false, false, true, None),
      _ => {
        return Err(ClientError::UnexpectedPresentationAcknowledgement {
          expected: pending_presentation_name(pending).into(),
          actual: presentation_acknowledgement_name(acknowledgement).into(),
        });
      }
    };
    let _popped = self.pending_presentations.pop_front();
    if applied_checkpoint {
      self.renderer_requires_checkpoint = false;
    }
    if incompatible_checkpoint {
      self.renderer_requires_checkpoint = true;
    }
    if incompatible_geometry {
      self.renderer_requires_checkpoint = true;
    }
    let checkpoint_is_pending = self
      .pending_presentations
      .iter()
      .any(|pending| matches!(pending, PendingPresentation::Checkpoint { .. }));
    self.state.set_resume_sequence(
      (!self.renderer_requires_checkpoint && !checkpoint_is_pending)
        .then_some(acknowledged_sequence)
        .flatten(),
    );
    Ok(delivery_progress)
  }

  fn accept_presentation_acknowledgement_request(
    &mut self,
    request: PresentationAcknowledgement,
  ) -> Result<Option<u64>, ClientError> {
    let result = self.accept_presentation_acknowledgement(request.acknowledgement);
    let completion = match &result {
      Ok(_) => Ok(()),
      Err(ClientError::UnexpectedPresentationAcknowledgement { expected, actual }) => {
        Err(AttachmentAcknowledgementError::Rejected {
          expected: expected.clone(),
          actual: actual.clone(),
        })
      }
      Err(_) => Err(AttachmentAcknowledgementError::Closed),
    };
    let _ignored = request.completion.send(completion);
    result
  }

  fn finish(&mut self, reason: AttachExitReason) -> AttachExit {
    let exit = AttachExit {
      reason,
      next_sequence: self.state.next_sequence(),
      received_sequence: self.state.received_sequence(),
    };
    let _ignored = self
      .events
      .try_send(AttachmentEvent::Exited { exit: exit.clone() });
    exit
  }
}

enum IncomingServerMessage {
  Message(Box<ServerMessage>),
  ConnectionClosed,
  Fatal(ClientError),
}

enum ControllerAction {
  Continue,
  Exit { reason: AttachExitReason },
}

enum WriterStatus {
  DetachSent,
  ConnectionClosed,
  Fatal(ClientError),
}

fn pending_presentation_name(pending: PendingPresentation) -> &'static str {
  match pending {
    PendingPresentation::Output { .. } => "output",
    PendingPresentation::Checkpoint { .. } => "checkpoint",
    PendingPresentation::Geometry { .. } => "pty geometry",
  }
}

fn presentation_acknowledgement_name(acknowledgement: PresentationAck) -> &'static str {
  match acknowledgement {
    PresentationAck::Output { .. } => "output acknowledgement",
    PresentationAck::Checkpoint { .. } => "checkpoint acknowledgement",
    PresentationAck::CheckpointIncompatible { .. } => "incompatible checkpoint acknowledgement",
    PresentationAck::Geometry { .. } => "pty geometry acknowledgement",
    PresentationAck::GeometryIncompatible { .. } => "incompatible pty geometry acknowledgement",
  }
}

struct AttachmentWriterChannels {
  commands: mpsc::Receiver<AttachmentCommand>,
  presentation_progress: watch::Receiver<Option<u64>>,
  history_requests: watch::Receiver<Option<ClientMessage>>,
  statuses: mpsc::UnboundedSender<WriterStatus>,
}

/// Read-side EOF and terminal messages remain authoritative after writes stop.
/// A closed status channel must stay pending rather than spin or discard a
/// presentation event that is waiting for its consumer.
async fn writer_failure(statuses: &mut mpsc::UnboundedReceiver<WriterStatus>) -> ClientError {
  loop {
    match statuses.recv().await {
      Some(WriterStatus::Fatal(error)) => return error,
      Some(WriterStatus::DetachSent) => {}
      Some(WriterStatus::ConnectionClosed) | None => return std::future::pending().await,
    }
  }
}

async fn drive_attachment_writer<W>(
  mut writer: W,
  state: AttachmentState,
  liveness: AttachmentLiveness,
  options: AttachmentControllerOptions,
  mut channels: AttachmentWriterChannels,
) where
  W: AsyncWrite + Unpin,
{
  let now = Instant::now();
  let mut heartbeats = interval_at(
    now + liveness.heartbeat_interval,
    liveness.heartbeat_interval,
  );
  heartbeats.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut heartbeat_nonce = 0_u64;
  let mut presentation_progress_open = true;
  let mut history_requests_open = true;
  let mut resize_after_layout_reacquire = options.reacquire_layout_lease
    && options.resize_after_layout_reacquire.is_some()
    && !state.lease_status(LeaseKind::Layout).owned_by_client;

  let status = loop {
    tokio::select! {
      // A busy local input producer must not indefinitely postpone the
      // negotiated liveness heartbeat. Once due, send it before another
      // queued command; between ticks, commands remain responsive.
      biased;
      _ = heartbeats.tick() => {
        match send_writer_heartbeat(
          &mut writer,
          &state,
          &options,
          &mut resize_after_layout_reacquire,
          &mut heartbeat_nonce,
        )
        .await
        {
          Ok(true) => {}
          Ok(false) => break WriterStatus::ConnectionClosed,
          Err(error) => break WriterStatus::Fatal(error),
        }
      }
      changed = channels.presentation_progress.changed(), if presentation_progress_open => {
        if changed.is_err() {
          presentation_progress_open = false;
          continue;
        }
        let Some(sequence) = *channels.presentation_progress.borrow_and_update() else {
          continue;
        };
        match send_attachment_message(
          &mut writer,
          &ClientMessage::PresentationApplied { sequence },
        )
        .await
        {
          Ok(true) => {}
          Ok(false) => break WriterStatus::ConnectionClosed,
          Err(error) => break WriterStatus::Fatal(error),
        }
      }
      command = channels.commands.recv() => {
        match command {
          Some(AttachmentCommand::Detach) | None => {
            match send_attachment_message(&mut writer, &ClientMessage::Detach).await {
              Ok(true) => break WriterStatus::DetachSent,
              Ok(false) => break WriterStatus::ConnectionClosed,
              Err(error) => break WriterStatus::Fatal(error),
            }
          }
          Some(command) => {
            match send_attachment_command(&mut writer, command).await {
              Ok(true) => {}
              Ok(false) => break WriterStatus::ConnectionClosed,
              Err(error) => break WriterStatus::Fatal(error),
            }
          }
        }
      }
      changed = channels.history_requests.changed(), if history_requests_open => {
        if changed.is_err() {
          history_requests_open = false;
          continue;
        }
        let message = channels.history_requests.borrow_and_update().clone();
        if let Some(message) = message {
          match send_attachment_message(&mut writer, &message).await {
            Ok(true) => {}
            Ok(false) => break WriterStatus::ConnectionClosed,
            Err(error) => break WriterStatus::Fatal(error),
          }
        }
      }
    }
  };
  let detach_sent = matches!(status, WriterStatus::DetachSent);
  let _ignored = channels.statuses.send(status);
  if detach_sent {
    // Keep the write task and status channel alive until the reader observes
    // the daemon's detach acknowledgement. The controller aborts this task
    // when it finishes.
    std::future::pending::<()>().await;
  }
}

fn last_peer_activity(peer_activity: &Mutex<Instant>) -> Instant {
  match peer_activity.lock() {
    Ok(activity) => *activity,
    Err(poisoned) => *poisoned.into_inner(),
  }
}

async fn wait_for_peer_silence(peer_activity: Arc<Mutex<Instant>>, peer_timeout: Duration) {
  loop {
    tokio::time::sleep_until(last_peer_activity(&peer_activity) + peer_timeout).await;
    if Instant::now().saturating_duration_since(last_peer_activity(&peer_activity)) >= peer_timeout
    {
      return;
    }
  }
}

async fn send_attachment_command<W>(
  writer: &mut W,
  command: AttachmentCommand,
) -> Result<bool, ClientError>
where
  W: AsyncWrite + Unpin,
{
  let message = match command {
    AttachmentCommand::Input { data } => ClientMessage::Input { data },
    AttachmentCommand::Resize { terminal_size } => ClientMessage::Resize { terminal_size },
    AttachmentCommand::AcquireLease { lease } => ClientMessage::AcquireLease { lease },
    AttachmentCommand::ReleaseLease { lease } => ClientMessage::ReleaseLease { lease },
    AttachmentCommand::RequestCheckpoint => ClientMessage::RequestCheckpoint,
    AttachmentCommand::Detach => unreachable!("detach exits before command conversion"),
  };
  send_attachment_message(writer, &message).await
}

async fn send_writer_heartbeat<W>(
  writer: &mut W,
  state: &AttachmentState,
  options: &AttachmentControllerOptions,
  resize_after_layout_reacquire: &mut bool,
  heartbeat_nonce: &mut u64,
) -> Result<bool, ClientError>
where
  W: AsyncWrite + Unpin,
{
  if options.reacquire_input_lease
    && !state.lease_status(LeaseKind::Input).owned_by_client
    && !send_attachment_message(
      writer,
      &ClientMessage::AcquireLease {
        lease: LeaseKind::Input,
      },
    )
    .await?
  {
    return Ok(false);
  }
  if options.reacquire_layout_lease
    && !state.lease_status(LeaseKind::Layout).owned_by_client
    && !send_attachment_message(
      writer,
      &ClientMessage::AcquireLease {
        lease: LeaseKind::Layout,
      },
    )
    .await?
  {
    return Ok(false);
  }
  if *resize_after_layout_reacquire
    && state.lease_status(LeaseKind::Layout).owned_by_client
    && let Some(terminal_size) = options.resize_after_layout_reacquire.clone()
  {
    if !send_attachment_message(writer, &ClientMessage::Resize { terminal_size }).await? {
      return Ok(false);
    }
    *resize_after_layout_reacquire = false;
  }

  *heartbeat_nonce = heartbeat_nonce.wrapping_add(1);
  send_attachment_message(
    writer,
    &ClientMessage::Heartbeat {
      nonce: *heartbeat_nonce,
    },
  )
  .await
}

async fn read_server_messages<R>(
  mut reader: R,
  sender: mpsc::Sender<IncomingServerMessage>,
  peer_activity: Arc<Mutex<Instant>>,
) where
  R: AsyncRead + Unpin,
{
  loop {
    let message = match read_frame::<_, ServerMessage>(&mut reader).await {
      Ok(Some(message)) => IncomingServerMessage::Message(Box::new(message)),
      Ok(None) | Err(CodecError::Io(_)) => IncomingServerMessage::ConnectionClosed,
      Err(error) => IncomingServerMessage::Fatal(error.into()),
    };
    if matches!(&message, IncomingServerMessage::Message(_)) {
      let mut last_activity = match peer_activity.lock() {
        Ok(activity) => activity,
        Err(poisoned) => poisoned.into_inner(),
      };
      *last_activity = Instant::now();
    }
    let terminal = !matches!(&message, IncomingServerMessage::Message(_));
    if sender.send(message).await.is_err() || terminal {
      return;
    }
  }
}

fn validate_checkpoint(checkpoint: &TerminalCheckpoint) -> Result<(), ClientError> {
  if checkpoint.is_supported() {
    Ok(())
  } else {
    Err(ClientError::UnsupportedCheckpoint {
      format: checkpoint.format.clone(),
      format_version: checkpoint.format_version,
    })
  }
}

fn validate_checkpoint_bundle(
  checkpoint: &TerminalCheckpoint,
  history: &TerminalHistorySnapshot,
) -> Result<(), ClientError> {
  validate_checkpoint(checkpoint)?;
  if !history.is_supported() {
    return Err(ClientError::UnsupportedTerminalHistory {
      format: history.format.clone(),
      format_version: history.format_version,
    });
  }
  if history.sequence != checkpoint.sequence {
    return Err(ClientError::InvalidTerminalHistorySequence {
      checkpoint_sequence: checkpoint.sequence,
      history_sequence: history.sequence,
    });
  }
  Ok(())
}

/// Runs the standard terminal presentation for an attached session.
///
/// The function never resizes the remote PTY. That action requires an
/// explicit layout lease and an explicit `resize` protocol message.
///
/// # Errors
///
/// Returns an error when local terminal I/O fails, a checkpoint is
/// unsupported, or the daemon sends an unexpected or fatal message.
pub async fn attach_interactive<S>(
  stream: S,
  attached: &AttachedSession,
) -> Result<AttachExit, ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  attach_interactive_with_options(stream, attached, InteractiveAttachOptions::default()).await
}

/// Runs the standard terminal presentation with optional automatic lease
/// recovery after a reconnect.
///
/// The function sends heartbeats using the negotiated cadence and closes the
/// local attachment if the daemon becomes silent for the negotiated timeout.
/// It never takes a lease from another attachment.
///
/// # Errors
///
/// Returns an error when local terminal I/O fails, a checkpoint is
/// unsupported, or the daemon sends an unexpected or fatal message.
pub async fn attach_interactive_with_options<S>(
  stream: S,
  attached: &AttachedSession,
  options: InteractiveAttachOptions,
) -> Result<AttachExit, ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  report_attachment(attached);
  let interactive = io::stdin().is_terminal();
  let _raw_mode = RawModeGuard::enable_if(interactive)?;
  let local_terminal_size = current_terminal_size();
  let controller_options = AttachmentControllerOptions {
    renderer_starts_compatible: terminal_grid_matches(
      &local_terminal_size,
      &attached.session.terminal_size,
    ),
    reacquire_input_lease: options.reacquire_input_lease,
    reacquire_layout_lease: options.reacquire_layout_lease,
    resize_after_layout_reacquire: options
      .resize_after_layout_reacquire
      .then_some(local_terminal_size.clone()),
    ..AttachmentControllerOptions::default()
  };
  let (controller, control, mut events) =
    AttachmentController::new(stream, attached, controller_options)?;
  let controller = controller.run();
  let input = forward_interactive_input(control.clone());
  let output = present_interactive_events(&mut events, &control);
  tokio::pin!(controller);
  tokio::pin!(input);
  tokio::pin!(output);

  tokio::select! {
    result = &mut controller => result,
    result = &mut input => {
      result?;
      controller.await
    }
    result = &mut output => {
      result?;
      controller.await
    }
  }
}

/// Returns the current local terminal size, or a portable 80x24 fallback.
#[must_use]
pub fn current_terminal_size() -> TerminalSize {
  let (columns, rows) = size().unwrap_or((80, 24));
  TerminalSize {
    columns,
    rows,
    pixel_width: 0,
    pixel_height: 0,
  }
}

fn terminal_grid_matches(left: &TerminalSize, right: &TerminalSize) -> bool {
  left.columns == right.columns && left.rows == right.rows
}

/// Restores a compatible terminal checkpoint to an asynchronous output.
///
/// The raw presenter emits a terminal reset and full-screen clear before the
/// checkpoint stream so the `ctmux_vt_state` initialization program never
/// inherits stale local screen or parser state.
///
/// # Errors
///
/// Returns an error when the checkpoint format is unsupported or output fails.
pub async fn restore_checkpoint<W>(
  output: &mut W,
  checkpoint: &TerminalCheckpoint,
) -> Result<(), ClientError>
where
  W: AsyncWrite + Unpin,
{
  if !checkpoint.is_supported() {
    return Err(ClientError::UnsupportedCheckpoint {
      format: checkpoint.format.clone(),
      format_version: checkpoint.format_version,
    });
  }
  output.write_all(CHECKPOINT_RENDERER_RESET).await?;
  output.write_all(&checkpoint.payload).await?;
  output.write_all(&checkpoint.input_prefix).await?;
  output.flush().await?;
  Ok(())
}

async fn read_response<S>(stream: &mut S) -> Result<ServerMessage, ClientError>
where
  S: AsyncRead + Unpin,
{
  match read_frame(stream).await? {
    Some(ServerMessage::Error { code, message }) => Err(ClientError::Server { code, message }),
    Some(message) => Ok(message),
    None => Err(ClientError::UnexpectedEof),
  }
}

fn attachment_liveness(
  heartbeat_interval_ms: u64,
  attachment_liveness_timeout_ms: u64,
) -> Result<AttachmentLiveness, ClientError> {
  if heartbeat_interval_ms == 0
    || attachment_liveness_timeout_ms == 0
    || heartbeat_interval_ms >= attachment_liveness_timeout_ms
  {
    return Err(ClientError::InvalidAttachmentLiveness {
      heartbeat_interval_ms,
      attachment_liveness_timeout_ms,
    });
  }

  Ok(AttachmentLiveness {
    heartbeat_interval: Duration::from_millis(heartbeat_interval_ms),
    peer_timeout: Duration::from_millis(attachment_liveness_timeout_ms),
  })
}

fn report_attachment(attached: &AttachedSession) {
  if attached.history_gap && attached.checkpoint.is_none() {
    eprintln!(
      "ctmux: older scrollback is no longer retained; restoring sequence {}",
      attached.replay_from
    );
  }
  if attached.terminal_size_mismatch {
    let terminal_size = current_terminal_size();
    eprintln!(
      "ctmux: terminal is {}x{}, but this session is {}x{}; the PTY will not be resized",
      terminal_size.columns,
      terminal_size.rows,
      attached.session.terminal_size.columns,
      attached.session.terminal_size.rows,
    );
  }
  if !attached.input_lease.owned_by_client {
    let reason = if attached.input_lease.held {
      "another attachment owns input"
    } else {
      "input was not requested"
    };
    eprintln!("ctmux: view-only attachment ({reason})");
  }
  if !attached.layout_lease.owned_by_client && attached.layout_lease.held {
    eprintln!("ctmux: another attachment owns PTY layout");
  }
  eprintln!(
    "[attached to {}; press Ctrl-] to detach]",
    attached.session.name
  );
}

async fn forward_interactive_input(control: AttachmentControl) -> Result<(), ClientError> {
  let mut stdin = tokio::io::stdin();
  let mut buffer = vec![0_u8; 4096];
  let mut reported_view_only = false;

  loop {
    let bytes_read = stdin.read(&mut buffer).await?;
    if bytes_read == 0 {
      return detach_interactive(&control).await;
    }
    let input = &buffer[..bytes_read];
    if let Some(detach_at) = input.iter().position(|byte| *byte == DETACH_BYTE) {
      if detach_at > 0 {
        submit_interactive_input(
          &control,
          input[..detach_at].to_vec(),
          &mut reported_view_only,
        )
        .await?;
      }
      return detach_interactive(&control).await;
    }
    submit_interactive_input(&control, input.to_vec(), &mut reported_view_only).await?;
  }
}

async fn detach_interactive(control: &AttachmentControl) -> Result<(), ClientError> {
  match control.detach().await {
    Ok(()) | Err(AttachmentCommandError::Closed) => Ok(()),
    Err(error) => Err(error.into()),
  }
}

async fn submit_interactive_input(
  control: &AttachmentControl,
  input: Vec<u8>,
  reported_view_only: &mut bool,
) -> Result<(), ClientError> {
  match control.input(input).await {
    Ok(()) => {
      *reported_view_only = false;
      Ok(())
    }
    Err(AttachmentCommandError::InputLeaseRequired) => {
      if !*reported_view_only {
        eprintln!("\r\n[view-only attachment; press Ctrl-] to detach]");
        *reported_view_only = true;
      }
      Ok(())
    }
    Err(AttachmentCommandError::Closed) => Ok(()),
    Err(error) => Err(error.into()),
  }
}

async fn present_interactive_events(
  events: &mut AttachmentEvents,
  control: &AttachmentControl,
) -> Result<(), ClientError> {
  let mut stdout = tokio::io::stdout();
  while let Some(event) = events.recv().await {
    match event {
      AttachmentEvent::Checkpoint {
        checkpoint,
        history: _,
        history_manifest: _,
        history_gap,
      } => {
        if history_gap {
          eprintln!("\r\n[older scrollback is no longer retained; terminal state restored]");
        }
        restore_checkpoint(&mut stdout, &checkpoint).await?;
        if terminal_grid_matches(&current_terminal_size(), &checkpoint.terminal_size) {
          let _ignored = control.acknowledge_checkpoint(checkpoint.sequence).await;
        } else {
          eprintln!(
            "\r\n[checkpoint grid is {}x{}, but this terminal differs; a reconnect will restore another checkpoint]",
            checkpoint.terminal_size.columns, checkpoint.terminal_size.rows
          );
          let _ignored = control
            .acknowledge_checkpoint_incompatible(checkpoint.sequence)
            .await;
        }
      }
      AttachmentEvent::Output {
        sequence_end, data, ..
      } => {
        stdout.write_all(&data).await?;
        stdout.flush().await?;
        let _ignored = control.acknowledge_output(sequence_end).await;
      }
      AttachmentEvent::PtyGeometryChanged {
        terminal_size,
        observed_sequence,
      } => {
        eprintln!(
          "\r\n[PTY layout is now {}x{}; this local terminal was not resized, so a reconnect will restore a checkpoint]",
          terminal_size.columns, terminal_size.rows
        );
        let _ignored = control
          .acknowledge_geometry_incompatible(observed_sequence)
          .await;
      }
      AttachmentEvent::LeaseStatus { lease, status } => {
        let owner = if status.owned_by_client {
          "owned by this attachment"
        } else if status.held {
          "owned by another attachment"
        } else {
          "available"
        };
        eprintln!("\r\n[{} lease is {owner}]", lease_name(lease));
      }
      AttachmentEvent::ShellStateChanged { .. }
      | AttachmentEvent::HistorySynced { .. }
      | AttachmentEvent::HeartbeatAck { .. }
      | AttachmentEvent::Exited { .. } => {}
      AttachmentEvent::ServerError { message, .. } => eprintln!("\r\n[ctmux: {message}]"),
      AttachmentEvent::SessionEnded { exit_code, .. } => {
        stdout.flush().await?;
        eprintln!("\r\n[session ended with exit code {exit_code:?}]");
      }
    }
  }
  Ok(())
}

async fn send_attachment_message<W>(
  writer: &mut W,
  message: &ClientMessage,
) -> Result<bool, ClientError>
where
  W: AsyncWrite + Unpin,
{
  match write_frame(writer, message).await {
    Ok(()) => Ok(true),
    Err(CodecError::Io(_)) => Ok(false),
    Err(error) => Err(error.into()),
  }
}

fn lease_name(lease: LeaseKind) -> &'static str {
  match lease {
    LeaseKind::Input => "input",
    LeaseKind::Layout => "layout",
  }
}

fn unexpected(expected: &'static str, response: &ServerMessage) -> ClientError {
  ClientError::UnexpectedResponse {
    expected,
    actual: format!("{response:?}"),
  }
}

struct RawModeGuard {
  enabled: bool,
}

impl RawModeGuard {
  fn enable_if(enabled: bool) -> Result<Self, ClientError> {
    if enabled {
      enable_raw_mode()?;
    }
    Ok(Self { enabled })
  }
}

impl Drop for RawModeGuard {
  fn drop(&mut self) {
    if self.enabled {
      let _ignored = disable_raw_mode();
    }
  }
}

#[derive(Debug, Error)]
pub enum ClientError {
  #[error(transparent)]
  Codec(#[from] CodecError),
  #[error("terminal I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("daemon closed the connection before responding")]
  UnexpectedEof,
  #[error(
    "server announced invalid attachment liveness settings: heartbeat {heartbeat_interval_ms}ms, timeout {attachment_liveness_timeout_ms}ms"
  )]
  InvalidAttachmentLiveness {
    heartbeat_interval_ms: u64,
    attachment_liveness_timeout_ms: u64,
  },
  #[error(
    "attachment controller queue capacities must both be non-zero (commands {command_queue_capacity}, events {event_queue_capacity})"
  )]
  InvalidAttachmentQueueCapacity {
    command_queue_capacity: usize,
    event_queue_capacity: usize,
  },
  #[error("attachment controller has already been started")]
  AttachmentControllerAlreadyRun,
  #[error("daemon error {code:?}: {message}")]
  Server { code: ErrorCode, message: String },
  #[error(transparent)]
  AttachmentCommand(#[from] AttachmentCommandError),
  #[error("unsupported terminal checkpoint format {format} version {format_version}")]
  UnsupportedCheckpoint { format: String, format_version: u16 },
  #[error("a terminal checkpoint and its history snapshot must be delivered together")]
  CheckpointHistoryPresenceMismatch,
  #[error("unsupported terminal history format {format} version {format_version}")]
  UnsupportedTerminalHistory { format: String, format_version: u16 },
  #[error(
    "terminal history sequence {history_sequence} does not match checkpoint sequence {checkpoint_sequence}"
  )]
  InvalidTerminalHistorySequence {
    checkpoint_sequence: u64,
    history_sequence: u64,
  },
  #[error("invalid history snapshot: {0}")]
  InvalidHistorySnapshot(String),
  #[error(
    "initial checkpoint sequence {checkpoint_sequence} does not match replay start {replay_from}"
  )]
  InvalidInitialCheckpointSequence {
    checkpoint_sequence: u64,
    replay_from: u64,
  },
  #[error(
    "invalid output frame: expected start {expected_sequence}, got [{sequence_start}, {sequence_end}) for {data_len} bytes"
  )]
  InvalidOutputSequence {
    expected_sequence: u64,
    sequence_start: u64,
    sequence_end: u64,
    data_len: usize,
  },
  #[error(
    "checkpoint sequence {checkpoint_sequence} regresses previously received sequence {previous_sequence}"
  )]
  StaleCheckpoint {
    checkpoint_sequence: u64,
    previous_sequence: u64,
  },
  #[error(
    "PTY geometry at raw sequence {observed_sequence} arrived before expected raw output through {expected_sequence}"
  )]
  GeometryAheadOfOutput {
    expected_sequence: u64,
    observed_sequence: u64,
  },
  #[error("unexpected presentation acknowledgement {actual}; expected {expected}")]
  UnexpectedPresentationAcknowledgement { expected: String, actual: String },
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Hold readable final frames until a renderer acknowledgement encounters
  /// the peer's closed write side. This makes the half-close ordering exact.
  struct FinalFramesAfterWriteFailure {
    stream: tokio::io::DuplexStream,
    read_gate: Option<oneshot::Receiver<()>>,
    write_failed: Option<oneshot::Sender<()>>,
    force_write_failure: bool,
  }

  impl AsyncRead for FinalFramesAfterWriteFailure {
    fn poll_read(
      mut self: std::pin::Pin<&mut Self>,
      context: &mut std::task::Context<'_>,
      buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
      if let Some(gate) = &mut self.read_gate
        && std::future::Future::poll(std::pin::Pin::new(gate), context).is_pending()
      {
        return std::task::Poll::Pending;
      }
      self.read_gate = None;
      std::pin::Pin::new(&mut self.stream).poll_read(context, buffer)
    }
  }

  impl AsyncWrite for FinalFramesAfterWriteFailure {
    fn poll_write(
      mut self: std::pin::Pin<&mut Self>,
      context: &mut std::task::Context<'_>,
      buffer: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
      let result = if self.force_write_failure {
        std::task::Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)))
      } else {
        std::pin::Pin::new(&mut self.stream).poll_write(context, buffer)
      };
      if matches!(&result, std::task::Poll::Ready(Err(_)))
        && let Some(failed) = self.write_failed.take()
      {
        let _ = failed.send(());
      }
      result
    }

    fn poll_flush(
      mut self: std::pin::Pin<&mut Self>,
      context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
      std::pin::Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
      mut self: std::pin::Pin<&mut Self>,
      context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
      std::pin::Pin::new(&mut self.stream).poll_shutdown(context)
    }
  }

  #[tokio::test]
  async fn final_output_and_session_exit_survive_a_failed_renderer_acknowledgement_write() {
    let (stream, mut daemon) = tokio::io::duplex(4096);
    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 5,
        sequence_end: 8,
        data: b"end".to_vec(),
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::SessionEnded {
        session_id: "session-id".into(),
        exit_code: Some(7),
      },
    )
    .await
    .unwrap();
    drop(daemon);
    let (allow_read, read_gate) = oneshot::channel();
    let (write_failed, failed_write) = oneshot::channel();
    let stream = FinalFramesAfterWriteFailure {
      stream,
      read_gate: Some(read_gate),
      write_failed: Some(write_failed),
      force_write_failure: false,
    };
    let mut attached = attached_session(5, Some(checkpoint(5)), ShellState::default());
    attached.input_lease = LeaseStatus {
      held: true,
      owned_by_client: true,
    };
    let options = AttachmentControllerOptions {
      event_queue_capacity: 1,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(stream, &attached, options).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint { .. })
    ));
    control.acknowledge_checkpoint(5).await.unwrap();
    failed_write.await.unwrap();
    assert_eq!(
      control.input(b"ignored".to_vec()).await,
      Err(AttachmentCommandError::Closed)
    );
    let _ = allow_read.send(());
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_start: 5,
        sequence_end: 8,
        data: b"end".to_vec(),
      })
    );
    control.acknowledge_output(8).await.unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::SessionEnded {
        session_id: "session-id".into(),
        exit_code: Some(7),
      })
    );
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(
      exit.reason,
      AttachExitReason::SessionEnded { exit_code: Some(7) }
    );
    assert_eq!(exit.next_sequence, Some(8));
    assert_eq!(exit.received_sequence, 8);
  }

  #[tokio::test(start_paused = true)]
  async fn failed_writer_drain_is_bounded_even_while_the_peer_keeps_sending() {
    let (stream, mut daemon) = tokio::io::duplex(4096);
    let (write_failed, failed_write) = oneshot::channel();
    let stream = FinalFramesAfterWriteFailure {
      stream,
      read_gate: None,
      write_failed: Some(write_failed),
      force_write_failure: true,
    };
    let mut attached = attached_session(5, None, ShellState::default());
    attached.input_lease = LeaseStatus {
      held: true,
      owned_by_client: true,
    };
    attached.liveness = AttachmentLiveness {
      heartbeat_interval: Duration::from_millis(10),
      peer_timeout: Duration::from_millis(30),
    };
    let (controller, control, mut events) =
      AttachmentController::new(stream, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    control.input(b"failed write".to_vec()).await.unwrap();
    failed_write.await.unwrap();
    for nonce in 0..2 {
      tokio::time::advance(Duration::from_millis(10)).await;
      write_frame(&mut daemon, &ServerMessage::HeartbeatAck { nonce })
        .await
        .unwrap();
      assert_eq!(
        events.recv().await,
        Some(AttachmentEvent::HeartbeatAck { nonce })
      );
      assert!(!runner.is_finished());
    }
    // Only ten milliseconds remain in the write-failure drain window; the
    // fresh incoming frame would postpone the ordinary silence timer by 30.
    let exit = tokio::time::timeout(Duration::from_millis(15), runner)
      .await
      .expect("incoming activity prolonged the failed-writer drain")
      .unwrap()
      .unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(5));
  }

  #[tokio::test]
  async fn begin_attach_works_over_a_generic_duplex_stream() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let expected_shell_state = shell_state(4, "/workspace");
    let server_shell_state = expected_shell_state.clone();
    let server = tokio::spawn(async move {
      let handshake: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
      assert!(matches!(
        handshake,
        ClientMessage::Handshake {
          protocol_version: PROTOCOL_VERSION,
          ..
        }
      ));
      write_frame(
        &mut daemon,
        &ServerMessage::HandshakeAccepted {
          protocol_version: PROTOCOL_VERSION,
          server_version: "test".into(),
          build: None,
          heartbeat_interval_ms: 1_000,
          attachment_liveness_timeout_ms: 3_000,
        },
      )
      .await
      .unwrap();

      let attach: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
      assert!(matches!(
        attach,
        ClientMessage::AttachSession {
          request_input_lease: true,
          request_layout_lease: false,
          request_command_line: true,
          request_running_command: true,
          ..
        }
      ));
      write_frame(
        &mut daemon,
        &ServerMessage::Attached {
          attachment_token: "token".into(),
          session: session_info(),
          earliest_sequence: 0,
          next_sequence: 0,
          replay_from: 0,
          history_gap: false,
          history_manifest: None,
          checkpoint: None,
          history: None,
          terminal_size_mismatch: false,
          input_lease: LeaseStatus {
            held: true,
            owned_by_client: true,
          },
          layout_lease: LeaseStatus {
            held: false,
            owned_by_client: false,
          },
          shell_state: server_shell_state,
        },
      )
      .await
      .unwrap();
    });

    let (stream, attached) = begin_attach(
      client,
      &ClientIdentity {
        name: "test-client".into(),
        version: "test".into(),
      },
      AttachRequest {
        session: "work".into(),
        resume_from: Some(12),
        terminal_size: TerminalSize::default(),
        request_input_lease: true,
        request_layout_lease: false,
        request_command_line: true,
        request_running_command: true,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      },
    )
    .await
    .unwrap();

    assert_eq!(attached.session.name, "work");
    assert_eq!(attached.handshake_info.server_version, "test");
    assert_eq!(attached.handshake_info.protocol_version, PROTOCOL_VERSION);
    assert!(attached.handshake_info.build.is_none());
    assert!(attached.input_lease.owned_by_client);
    assert_eq!(attached.shell_state, expected_shell_state);
    assert_eq!(
      attached.shell_state_cache().snapshot(),
      expected_shell_state
    );
    assert_eq!(attached.liveness.heartbeat_interval, Duration::from_secs(1));
    assert_eq!(attached.liveness.peer_timeout, Duration::from_secs(3));
    drop(stream);
    server.await.unwrap();
  }

  #[test]
  fn shell_state_cache_applies_only_newer_revisions() {
    let cache = ShellStateCache::new(shell_state(4, "/before"));

    assert!(!cache.apply_if_newer(shell_state(3, "/older")));
    assert!(!cache.apply_if_newer(shell_state(4, "/same")));
    assert_eq!(cache.snapshot(), shell_state(4, "/before"));

    assert!(cache.apply_if_newer(shell_state(5, "/after")));
    assert_eq!(cache.snapshot(), shell_state(5, "/after"));
  }

  #[test]
  fn controller_rejects_a_checkpoint_without_its_history_snapshot() {
    let (client, _daemon) = tokio::io::duplex(64);
    let mut attached = attached_session(7, Some(checkpoint(7)), ShellState::default());
    attached.history = None;

    assert!(matches!(
      AttachmentController::new(client, &attached, controller_options()),
      Err(ClientError::CheckpointHistoryPresenceMismatch)
    ));
  }

  #[test]
  fn controller_rejects_history_from_a_different_checkpoint_boundary() {
    let (client, _daemon) = tokio::io::duplex(64);
    let mut attached = attached_session(7, Some(checkpoint(7)), ShellState::default());
    attached
      .history
      .as_mut()
      .expect("checkpoint fixture includes history")
      .sequence = 6;

    assert!(matches!(
      AttachmentController::new(client, &attached, controller_options()),
      Err(ClientError::InvalidTerminalHistorySequence {
        checkpoint_sequence: 7,
        history_sequence: 6,
      })
    ));
  }

  #[test]
  fn shell_state_cache_is_shared_across_threads() {
    let cache = ShellStateCache::new(shell_state(4, "/before"));
    let worker_cache = cache.clone();

    let did_update =
      std::thread::spawn(move || worker_cache.apply_if_newer(shell_state(5, "/after")))
        .join()
        .unwrap();

    assert!(did_update);
    assert_eq!(cache.snapshot(), shell_state(5, "/after"));
  }

  #[tokio::test]
  async fn shell_state_updates_do_not_advance_renderer_resume_sequence() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(73, None, shell_state(4, "/before"));
    let (controller, _control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let updated_state = shell_state(5, "/after");
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::ShellStateChanged {
        state: updated_state.clone(),
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::ShellStateChanged {
        state: updated_state.clone(),
      })
    );
    assert_eq!(state.received_sequence(), 73);
    assert_eq!(state.resume_sequence(), Some(73));
    assert_eq!(state.shell_state_cache().snapshot(), updated_state);

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(73));
    assert_eq!(exit.received_sequence, 73);
  }

  #[tokio::test]
  async fn controller_waits_for_detach_acknowledgement() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, _events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let mut runner = tokio::spawn(controller.run());

    control.detach().await.unwrap();
    assert_eq!(
      read_frame::<_, ClientMessage>(&mut daemon)
        .await
        .unwrap()
        .unwrap(),
      ClientMessage::Detach
    );
    assert!(
      tokio::time::timeout(Duration::from_millis(20), &mut runner)
        .await
        .is_err(),
      "controller exited before the daemon acknowledged detach"
    );

    write_frame(&mut daemon, &ServerMessage::Detached)
      .await
      .unwrap();
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::Detached);
  }

  #[tokio::test]
  async fn controller_uses_only_renderer_acknowledged_output_for_resume() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();

    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      })
    );
    assert_eq!(state.received_sequence(), 3);
    assert_eq!(state.resume_sequence(), Some(0));

    control.acknowledge_output(3).await.unwrap();
    assert_eq!(state.resume_sequence(), Some(3));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(3));
    assert_eq!(exit.received_sequence, 3);
  }

  #[tokio::test]
  async fn renderer_acknowledgement_waits_until_controller_applies_it() {
    let (client, daemon) = tokio::io::duplex(4096);
    let checkpoint = checkpoint(7);
    let attached = attached_session(7, Some(checkpoint.clone()), ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let acknowledgement =
      tokio::spawn(async move { control.acknowledge_checkpoint(checkpoint.sequence).await });

    tokio::task::yield_now().await;
    assert!(
      !acknowledgement.is_finished(),
      "acknowledgement completed before the controller was running"
    );

    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint { .. })
    ));
    acknowledgement.await.unwrap().unwrap();
    assert_eq!(state.resume_sequence(), Some(7));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(7));
  }

  #[tokio::test]
  async fn rejected_renderer_acknowledgement_is_returned_to_its_caller() {
    let (client, daemon) = tokio::io::duplex(4096);
    let checkpoint = checkpoint(7);
    let attached = attached_session(7, Some(checkpoint), ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint { .. })
    ));

    let error = control.acknowledge_output(7).await.unwrap_err();
    assert_eq!(
      error,
      AttachmentAcknowledgementError::Rejected {
        expected: "checkpoint".into(),
        actual: "output acknowledgement".into(),
      }
    );
    assert!(matches!(
      runner.await.unwrap(),
      Err(ClientError::UnexpectedPresentationAcknowledgement { .. })
    ));
    drop(daemon);
  }

  #[tokio::test]
  async fn recovery_checkpoint_invalidates_resume_until_renderer_acknowledges_it() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();
    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output event");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    assert_eq!(state.resume_sequence(), Some(3));

    let checkpoint = checkpoint(10);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: Box::new(terminal_history(checkpoint.sequence)),
        history_gap: true,
        history_manifest: None,
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: terminal_history(checkpoint.sequence),
        history_gap: true,
        history_manifest: None,
      })
    );
    assert_eq!(state.received_sequence(), 10);
    assert_eq!(state.resume_sequence(), None);

    control
      .acknowledge_checkpoint(checkpoint.sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), Some(10));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(10));
  }

  #[tokio::test]
  async fn geometry_at_checkpoint_sequence_is_delivered_and_acknowledged() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let runner = tokio::spawn(controller.run());
    let checkpoint = checkpoint(10);
    let terminal_size = TerminalSize {
      columns: 132,
      rows: 48,
      pixel_width: 0,
      pixel_height: 0,
    };

    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: Box::new(terminal_history(checkpoint.sequence)),
        history_gap: true,
        history_manifest: None,
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: terminal_history(checkpoint.sequence),
        history_gap: true,
        history_manifest: None,
      })
    );
    control
      .acknowledge_checkpoint(checkpoint.sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), Some(checkpoint.sequence));

    write_frame(
      &mut daemon,
      &ServerMessage::PtyGeometryChanged {
        terminal_size: terminal_size.clone(),
        observed_sequence: checkpoint.sequence,
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::PtyGeometryChanged {
        terminal_size: terminal_size.clone(),
        observed_sequence: checkpoint.sequence,
      })
    );
    assert_eq!(state.terminal_size(), terminal_size);
    control
      .acknowledge_geometry(checkpoint.sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), Some(checkpoint.sequence));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(checkpoint.sequence));
  }

  #[tokio::test]
  async fn geometry_acknowledgement_preserves_order_without_advancing_raw_resume() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let runner = tokio::spawn(controller.run());
    let terminal_size = TerminalSize {
      columns: 132,
      rows: 48,
      pixel_width: 0,
      pixel_height: 0,
    };

    write_frame(
      &mut daemon,
      &ServerMessage::PtyGeometryChanged {
        terminal_size: terminal_size.clone(),
        observed_sequence: 0,
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();

    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::PtyGeometryChanged {
        terminal_size: terminal_size.clone(),
        observed_sequence: 0,
      })
    );
    assert_eq!(state.terminal_size(), terminal_size);
    control.acknowledge_geometry(0).await.unwrap();
    assert_eq!(state.resume_sequence(), Some(0));

    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output after geometry event");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    assert_eq!(state.resume_sequence(), Some(3));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(3));
  }

  #[tokio::test]
  async fn incompatible_geometry_keeps_resume_empty_after_later_output() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let state = controller.state();
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::PtyGeometryChanged {
        terminal_size: TerminalSize {
          columns: 132,
          rows: 48,
          pixel_width: 0,
          pixel_height: 0,
        },
        observed_sequence: 0,
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();

    let Some(AttachmentEvent::PtyGeometryChanged {
      observed_sequence, ..
    }) = events.recv().await
    else {
      panic!("expected geometry event");
    };
    control
      .acknowledge_geometry_incompatible(observed_sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), None);

    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output after incompatible geometry");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    assert_eq!(state.resume_sequence(), None);

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, None);
    assert_eq!(exit.received_sequence, 3);
  }

  #[tokio::test]
  async fn incompatible_initial_grid_requires_a_compatible_checkpoint_before_resume() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let options = AttachmentControllerOptions {
      renderer_starts_compatible: false,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, options).unwrap();
    let state = controller.state();
    assert_eq!(state.resume_sequence(), None);
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();
    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    assert_eq!(state.resume_sequence(), None);
    loop {
      let message: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
      if matches!(message, ClientMessage::PresentationApplied { sequence: 3 }) {
        break;
      }
    }

    let checkpoint = checkpoint(3);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: Box::new(terminal_history(checkpoint.sequence)),
        history_gap: false,
        history_manifest: None,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint { .. })
    ));
    control
      .acknowledge_checkpoint_incompatible(checkpoint.sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), None);
    let repeated_progress = tokio::time::timeout(Duration::from_secs(1), async {
      loop {
        let message: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
        if matches!(message, ClientMessage::PresentationApplied { sequence: 3 }) {
          return;
        }
      }
    })
    .await;
    assert!(
      repeated_progress.is_ok(),
      "an incompatible checkpoint still replenishes delivery credit"
    );

    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: Box::new(terminal_history(checkpoint.sequence)),
        history_gap: false,
        history_manifest: None,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint { .. })
    ));
    control
      .acknowledge_checkpoint(checkpoint.sequence)
      .await
      .unwrap();
    assert_eq!(state.resume_sequence(), Some(checkpoint.sequence));

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(checkpoint.sequence));
  }

  fn paged_attachment(
    sequence: u64,
    snapshot_id: &str,
    rows: &[TerminalHistoryRow],
  ) -> (AttachedSession, Vec<u8>) {
    let (manifest, checkpoint, history, bytes) =
      history_sync::tests::fixture(sequence, snapshot_id, rows);
    let mut attached = attached_session(sequence, Some(checkpoint), ShellState::default());
    attached.history = Some(history);
    attached.history_manifest = Some(manifest);
    (attached, bytes)
  }

  async fn next_client_message(stream: &mut tokio::io::DuplexStream) -> ClientMessage {
    tokio::time::timeout(Duration::from_secs(1), read_frame(stream))
      .await
      .expect("controller did not send its next message")
      .unwrap()
      .unwrap()
  }

  async fn next_attachment_event(events: &mut AttachmentEvents) -> AttachmentEvent {
    tokio::time::timeout(Duration::from_secs(1), events.recv())
      .await
      .expect("controller did not deliver its next event")
      .unwrap()
  }

  async fn expect_history_request(
    stream: &mut tokio::io::DuplexStream,
    snapshot_id: &str,
    sequence: u64,
  ) {
    assert_eq!(
      next_client_message(stream).await,
      ClientMessage::PresentationApplied { sequence }
    );
    assert!(matches!(next_client_message(stream).await,
      ClientMessage::HistoryRequest { snapshot_id: actual, offset: 0, .. } if actual == snapshot_id));
  }

  #[tokio::test]
  async fn history_waits_for_checkpoint_ack_and_never_advances_the_live_cursor() {
    let rows = [TerminalHistoryRow {
      text: "retained".into(),
      wrapped: false,
    }];
    let (attached, bytes) = paged_attachment(5, "initial", &rows);
    let (client, mut daemon) = tokio::io::duplex(4096);
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { .. }
    ));
    assert!(
      tokio::time::timeout(
        Duration::from_millis(10),
        read_frame::<_, ClientMessage>(&mut daemon)
      )
      .await
      .is_err()
    );
    control.acknowledge_checkpoint(5).await.unwrap();
    expect_history_request(&mut daemon, "initial", 5).await;

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 5,
        sequence_end: 8,
        data: b"new".to_vec(),
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::HistoryPage {
        snapshot_id: "initial".into(),
        offset: 0,
        data: bytes,
        next_offset: None,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Output {
        sequence_end: 8,
        ..
      }
    ));
    control.acknowledge_output(8).await.unwrap();
    assert!(matches!(next_attachment_event(&mut events).await,
      AttachmentEvent::HistorySynced { checkpoint, rows: actual, .. } if checkpoint.sequence == 5 && actual == rows));
    assert_eq!(control.state().received_sequence(), 8);
    assert_eq!(control.state().resume_sequence(), Some(8));
    drop(daemon);
    assert_eq!(runner.await.unwrap().unwrap().next_sequence, Some(8));
  }

  #[tokio::test]
  async fn late_pages_and_expiration_cannot_cancel_a_replacing_history_snapshot() {
    let old_rows = [TerminalHistoryRow {
      text: "old".into(),
      wrapped: false,
    }];
    let new_rows = [TerminalHistoryRow {
      text: "new".into(),
      wrapped: false,
    }];
    let (attached, _) = paged_attachment(5, "old", &old_rows);
    let (client, mut daemon) = tokio::io::duplex(4096);
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { .. }
    ));
    control.acknowledge_checkpoint(5).await.unwrap();
    expect_history_request(&mut daemon, "old", 5).await;

    let (manifest, checkpoint, history, bytes) = history_sync::tests::fixture(5, "new", &new_rows);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint,
        history: Box::new(history),
        history_manifest: Some(Box::new(manifest)),
        history_gap: false,
      },
    )
    .await
    .unwrap();
    assert!(matches!(next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { history_manifest: Some(manifest), .. } if manifest.snapshot_id == "new"));
    control.acknowledge_checkpoint(5).await.unwrap();
    expect_history_request(&mut daemon, "new", 5).await;
    // A malformed obsolete page must be ignored by identity before decoding.
    write_frame(
      &mut daemon,
      &ServerMessage::HistoryPage {
        snapshot_id: "old".into(),
        offset: 99,
        data: vec![0xff],
        next_offset: None,
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::HistorySnapshotExpired {
        snapshot_id: "old".into(),
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut daemon,
      &ServerMessage::HistoryPage {
        snapshot_id: "new".into(),
        offset: 0,
        data: bytes,
        next_offset: None,
      },
    )
    .await
    .unwrap();
    assert!(matches!(next_attachment_event(&mut events).await,
      AttachmentEvent::HistorySynced { snapshot_id, rows, .. } if snapshot_id == "new" && rows == new_rows));
    assert!(
      tokio::time::timeout(
        Duration::from_millis(10),
        read_frame::<_, ClientMessage>(&mut daemon)
      )
      .await
      .is_err(),
      "obsolete expiration requested another checkpoint"
    );
    drop(daemon);
    runner.await.unwrap().unwrap();
  }

  #[tokio::test]
  async fn current_history_expiration_requests_a_fresh_checkpoint_without_a_lease() {
    let rows = [TerminalHistoryRow {
      text: "retained".into(),
      wrapped: false,
    }];
    let (attached, _) = paged_attachment(5, "expired", &rows);
    let (client, mut daemon) = tokio::io::duplex(4096);
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { .. }
    ));
    control.acknowledge_checkpoint(5).await.unwrap();
    expect_history_request(&mut daemon, "expired", 5).await;
    write_frame(
      &mut daemon,
      &ServerMessage::HistorySnapshotExpired {
        snapshot_id: "expired".into(),
      },
    )
    .await
    .unwrap();
    assert_eq!(
      next_client_message(&mut daemon).await,
      ClientMessage::RequestCheckpoint
    );
    assert!(!control.state().leases().input.owned_by_client);
    assert!(!control.state().leases().layout.owned_by_client);

    let (manifest, checkpoint, history, bytes) = history_sync::tests::fixture(5, "fresh", &rows);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint,
        history: Box::new(history),
        history_manifest: Some(Box::new(manifest)),
        history_gap: false,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { .. }
    ));
    control.acknowledge_checkpoint(5).await.unwrap();
    expect_history_request(&mut daemon, "fresh", 5).await;
    write_frame(
      &mut daemon,
      &ServerMessage::HistoryPage {
        snapshot_id: "fresh".into(),
        offset: 0,
        data: bytes,
        next_offset: None,
      },
    )
    .await
    .unwrap();
    assert!(matches!(next_attachment_event(&mut events).await,
      AttachmentEvent::HistorySynced { snapshot_id, .. } if snapshot_id == "fresh"));
    drop(daemon);
    runner.await.unwrap().unwrap();
  }

  #[tokio::test]
  async fn a_full_advisory_queue_keeps_checkpoint_ack_live_and_history_precedes_exit() {
    let (attached, _) = paged_attachment(5, "empty", &[]);
    let (client, mut daemon) = tokio::io::duplex(4096);
    let options = AttachmentControllerOptions {
      event_queue_capacity: 2,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, options).unwrap();
    let runner = tokio::spawn(controller.run());
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::Checkpoint { .. }
    ));
    for nonce in 1..=2 {
      write_frame(&mut daemon, &ServerMessage::HeartbeatAck { nonce })
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), async {
      while events.receiver.len() != 2 {
        tokio::task::yield_now().await;
      }
    })
    .await
    .expect("advisory events did not fill the bounded queue");
    tokio::time::timeout(Duration::from_secs(1), control.acknowledge_checkpoint(5))
      .await
      .expect("full advisory queue blocked the checkpoint acknowledgement")
      .unwrap();
    // A final stream event is already readable while completed empty history
    // waits for the same full channel. Completion must survive normal exit.
    write_frame(
      &mut daemon,
      &ServerMessage::SessionEnded {
        session_id: "session-id".into(),
        exit_code: Some(0),
      },
    )
    .await
    .unwrap();
    for nonce in 1..=2 {
      assert_eq!(
        next_attachment_event(&mut events).await,
        AttachmentEvent::HeartbeatAck { nonce }
      );
    }
    assert!(matches!(next_attachment_event(&mut events).await,
      AttachmentEvent::HistorySynced { snapshot_id, rows, .. } if snapshot_id == "empty" && rows.is_empty()));
    assert!(matches!(
      next_attachment_event(&mut events).await,
      AttachmentEvent::SessionEnded { .. }
    ));
    assert_eq!(
      runner.await.unwrap().unwrap().reason,
      AttachExitReason::SessionEnded { exit_code: Some(0) }
    );
  }

  #[tokio::test]
  async fn acknowledgement_ledger_backpressures_until_the_renderer_advances() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let options = AttachmentControllerOptions {
      event_queue_capacity: 1,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, options).unwrap();
    let runner = tokio::spawn(controller.run());

    for (sequence_start, sequence_end, data) in [(0, 1, b"a".as_slice()), (1, 2, b"b".as_slice())] {
      write_frame(
        &mut daemon,
        &ServerMessage::Output {
          sequence_start,
          sequence_end,
          data: data.to_vec(),
        },
      )
      .await
      .unwrap();
    }

    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_end: 1,
        ..
      })
    ));
    assert!(events.try_recv().is_err());
    assert!(!runner.is_finished());

    control.acknowledge_output(1).await.unwrap();
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_end: 2,
        ..
      })
    ));
    control.acknowledge_output(2).await.unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
      loop {
        let message: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
        if let ClientMessage::PresentationApplied { sequence } = message
          && sequence == 2
        {
          return;
        }
      }
    })
    .await
    .expect("renderer progress was not sent to the daemon");

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(2));
  }

  #[tokio::test]
  async fn restore_checkpoint_resets_the_raw_renderer_before_its_state_stream() {
    let mut checkpoint = checkpoint(12);
    checkpoint.payload = b"state".to_vec();
    checkpoint.input_prefix = vec![0xe6];
    let mut expected = CHECKPOINT_RENDERER_RESET.to_vec();
    expected.extend_from_slice(&checkpoint.payload);
    expected.extend_from_slice(&checkpoint.input_prefix);

    let (mut writer, mut reader) = tokio::io::duplex(128);
    let restore = tokio::spawn(async move { restore_checkpoint(&mut writer, &checkpoint).await });
    let mut actual = vec![0; expected.len()];
    reader.read_exact(&mut actual).await.unwrap();
    restore.await.unwrap().unwrap();

    assert_eq!(actual, expected);
  }

  #[tokio::test]
  async fn full_presentation_queue_keeps_heartbeats_live_and_drains_after_progress() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let mut attached = attached_session(0, None, ShellState::default());
    attached.liveness = AttachmentLiveness {
      heartbeat_interval: Duration::from_millis(10),
      peer_timeout: Duration::from_millis(100),
    };
    let options = AttachmentControllerOptions {
      event_queue_capacity: 1,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, options).unwrap();
    let runner = tokio::spawn(controller.run());

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"abc".to_vec(),
      },
    )
    .await
    .unwrap();

    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_end: 3,
        ..
      })
    ));
    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 3,
        sequence_end: 6,
        data: b"def".to_vec(),
      },
    )
    .await
    .unwrap();

    let heartbeat = tokio::time::timeout(Duration::from_millis(100), async {
      loop {
        let message: ClientMessage = read_frame(&mut daemon)
          .await
          .unwrap()
          .expect("controller closed before heartbeat");
        if matches!(message, ClientMessage::Heartbeat { .. }) {
          return message;
        }
      }
    })
    .await
    .expect("controller stopped heartbeating while presentation queue was full");
    assert!(matches!(heartbeat, ClientMessage::Heartbeat { .. }));

    assert!(!runner.is_finished());
    control.acknowledge_output(3).await.unwrap();
    assert!(matches!(
      events.recv().await,
      Some(AttachmentEvent::Output {
        sequence_end: 6,
        ..
      })
    ));
    control.acknowledge_output(6).await.unwrap();

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(6));
    assert_eq!(exit.received_sequence, 6);
  }

  #[tokio::test(start_paused = true)]
  async fn silent_peer_disconnects_even_when_input_or_heartbeat_writes_are_blocked() {
    for send_input in [true, false] {
      // One byte of capacity guarantees every protocol frame backpressures
      // until the peer reads it. Keep the peer alive, but stop reading after
      // confirming that the writer entered either its input or heartbeat path.
      let (client, mut daemon) = tokio::io::duplex(1);
      let mut attached = attached_session(5, None, ShellState::default());
      attached.input_lease = LeaseStatus {
        held: true,
        owned_by_client: true,
      };
      attached.liveness = AttachmentLiveness {
        heartbeat_interval: Duration::from_millis(10),
        peer_timeout: Duration::from_millis(30),
      };
      let (controller, control, _events) =
        AttachmentController::new(client, &attached, controller_options()).unwrap();
      let mut runner = tokio::spawn(controller.run());
      if send_input {
        control.input(b"blocked input".to_vec()).await.unwrap();
      }
      daemon.read_u8().await.unwrap();

      let result = tokio::time::timeout(Duration::from_millis(100), &mut runner).await;
      if result.is_err() {
        runner.abort();
      }
      let exit = result
        .expect("a blocked write prevented peer-silence detection")
        .unwrap()
        .unwrap();
      assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
      assert_eq!(exit.next_sequence, Some(5));
      assert_eq!(exit.received_sequence, 5);
    }
  }

  #[tokio::test(start_paused = true)]
  async fn incoming_activity_keeps_a_backpressured_connection_alive() {
    let (client, mut daemon) = tokio::io::duplex(1);
    let mut attached = attached_session(5, None, ShellState::default());
    attached.input_lease = LeaseStatus {
      held: true,
      owned_by_client: true,
    };
    attached.liveness = AttachmentLiveness {
      heartbeat_interval: Duration::from_millis(10),
      peer_timeout: Duration::from_millis(30),
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, controller_options()).unwrap();
    let runner = tokio::spawn(controller.run());
    control.input(b"blocked input".to_vec()).await.unwrap();
    daemon.read_u8().await.unwrap();

    for nonce in 0..10 {
      tokio::time::advance(Duration::from_millis(15)).await;
      write_frame(&mut daemon, &ServerMessage::HeartbeatAck { nonce })
        .await
        .unwrap();
      assert_eq!(
        events.recv().await,
        Some(AttachmentEvent::HeartbeatAck { nonce })
      );
      assert!(!runner.is_finished());
    }
    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(5));
  }

  #[tokio::test]
  async fn fragmented_replay_larger_than_the_event_queue_drains_without_reconnect() {
    const OUTPUT_COUNT: u64 = 256;

    let (client, mut daemon) = tokio::io::duplex(64 * 1024);
    let attached = attached_session(0, None, ShellState::default());
    let options = AttachmentControllerOptions {
      event_queue_capacity: 4,
      ..controller_options()
    };
    let (controller, control, mut events) =
      AttachmentController::new(client, &attached, options).unwrap();
    let runner = tokio::spawn(controller.run());
    let daemon_task = tokio::spawn(async move {
      for sequence_start in 0..OUTPUT_COUNT {
        write_frame(
          &mut daemon,
          &ServerMessage::Output {
            sequence_start,
            sequence_end: sequence_start + 1,
            data: b"x".to_vec(),
          },
        )
        .await
        .unwrap();
      }

      loop {
        let message: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
        if matches!(
          message,
          ClientMessage::PresentationApplied {
            sequence: OUTPUT_COUNT
          }
        ) {
          return;
        }
      }
    });

    for expected_sequence in 1..=OUTPUT_COUNT {
      let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
        panic!("fragmented replay ended before sequence {expected_sequence}");
      };
      assert_eq!(sequence_end, expected_sequence);
      control.acknowledge_output(sequence_end).await.unwrap();
      if expected_sequence % 16 == 0 {
        tokio::task::yield_now().await;
      }
    }

    daemon_task.await.unwrap();
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(OUTPUT_COUNT));
    assert_eq!(exit.received_sequence, OUTPUT_COUNT);
  }

  #[tokio::test]
  async fn get_shell_state_uses_a_one_shot_request() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let expected_shell_state = shell_state(7, "/project");
    let server_shell_state = expected_shell_state.clone();
    let server = tokio::spawn(async move {
      let handshake: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
      assert!(matches!(
        handshake,
        ClientMessage::Handshake {
          protocol_version: PROTOCOL_VERSION,
          ..
        }
      ));
      write_frame(
        &mut daemon,
        &ServerMessage::HandshakeAccepted {
          protocol_version: PROTOCOL_VERSION,
          server_version: "test".into(),
          build: None,
          heartbeat_interval_ms: 1_000,
          attachment_liveness_timeout_ms: 3_000,
        },
      )
      .await
      .unwrap();

      let request: ClientMessage = read_frame(&mut daemon).await.unwrap().unwrap();
      assert!(matches!(
        request,
        ClientMessage::GetShellState { ref session } if session == "work"
      ));
      write_frame(
        &mut daemon,
        &ServerMessage::ShellStateResponse {
          session: session_info(),
          shell_state: server_shell_state,
        },
      )
      .await
      .unwrap();
    });

    let snapshot = get_shell_state(
      client,
      &ClientIdentity {
        name: "test-client".into(),
        version: "test".into(),
      },
      "work",
    )
    .await
    .unwrap();

    assert_eq!(snapshot.session.name, "work");
    assert_eq!(snapshot.shell_state, expected_shell_state);
    server.await.unwrap();
  }

  fn controller_options() -> AttachmentControllerOptions {
    AttachmentControllerOptions::default()
  }

  fn attached_session(
    replay_from: u64,
    checkpoint: Option<TerminalCheckpoint>,
    shell_state: ShellState,
  ) -> AttachedSession {
    let history = checkpoint
      .as_ref()
      .map(|checkpoint| terminal_history(checkpoint.sequence));
    AttachedSession {
      handshake_info: HandshakeInfo {
        server_version: "test".into(),
        protocol_version: PROTOCOL_VERSION,
        build: None,
        attachment_liveness: AttachmentLiveness {
          heartbeat_interval: Duration::from_mins(1),
          peer_timeout: Duration::from_mins(3),
        },
      },
      attachment_token: "token".into(),
      session: session_info(),
      replay_from,
      history_gap: false,
      history_manifest: None,
      checkpoint,
      history,
      terminal_size_mismatch: false,
      input_lease: LeaseStatus {
        held: false,
        owned_by_client: false,
      },
      layout_lease: LeaseStatus {
        held: false,
        owned_by_client: false,
      },
      shell_state_cache: ShellStateCache::new(shell_state.clone()),
      shell_state,
      liveness: AttachmentLiveness {
        heartbeat_interval: Duration::from_mins(1),
        peer_timeout: Duration::from_mins(3),
      },
    }
  }

  fn checkpoint(sequence: u64) -> TerminalCheckpoint {
    TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence,
      terminal_size: TerminalSize::default(),
      payload: Vec::new(),
      input_prefix: Vec::new(),
    }
  }

  fn terminal_history(sequence: u64) -> TerminalHistorySnapshot {
    TerminalHistorySnapshot {
      format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
      format_version: ctmux_proto::TERMINAL_HISTORY_FORMAT_VERSION,
      sequence,
      generation: 0,
      revision: 0,
      retained_bytes: 0,
      truncated: false,
      lines: Vec::new(),
    }
  }

  fn session_info() -> SessionInfo {
    SessionInfo {
      view_id: "view-test".into(),
      terminal_id: "terminal-test".into(),
      session_id: "session-id".into(),
      name: "work".into(),
      status: ctmux_proto::SessionStatus::Running,
      created_at_ms: 0,
      next_sequence: 0,
      terminal_size: TerminalSize::default(),
    }
  }

  fn shell_state(revision: u64, cwd: &str) -> ShellState {
    ShellState {
      revision,
      cwd: Some(cwd.into()),
      ..ShellState::default()
    }
  }
}
