use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use rmux_proto::{
  ClientMessage, CodecError, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, ShellState, TerminalCheckpoint, TerminalSize, read_frame, write_frame,
};
use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep};

const DETACH_BYTE: u8 = 0x1d;
// `rmux_vt_state` is an initialization stream, not an idempotent patch. The
// raw terminal presenter therefore starts every restore from a defined state.
const CHECKPOINT_RENDERER_RESET: &[u8] = b"\x1bc\x1b[2J\x1b[H";

/// Stable metadata sent during the `rmux` protocol handshake.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
  pub name: String,
  pub version: String,
}

/// Parameters for opening an attachment over an already-selected transport.
#[derive(Debug, Clone)]
pub struct AttachRequest {
  pub session: String,
  pub resume_from: Option<u64>,
  pub terminal_size: TerminalSize,
  pub request_input_lease: bool,
  pub request_layout_lease: bool,
  /// Request the current editable command line when daemon policy allows it.
  pub request_command_line: bool,
}

/// Session metadata and attachment-relative state returned by `rmuxd`.
#[derive(Debug, Clone)]
pub struct AttachedSession {
  pub session: SessionInfo,
  pub replay_from: u64,
  pub history_gap: bool,
  pub checkpoint: Option<TerminalCheckpoint>,
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

/// Metadata returned by a successful `rmux` handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandshakeInfo {
  pub attachment_liveness: AttachmentLiveness,
}

/// Optional automatic lease recovery for an interactive attachment.
///
/// This is useful when a reconnect begins while a stale attachment still owns
/// a lease. The client retries only leases it does not own; it never asks the
/// daemon to displace another attachment.
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
  /// Maximum time a full presentation queue may stall an attachment before the
  /// controller closes it. A later reconnect resumes from the renderer-applied
  /// cursor rather than silently discarding queued output. The controller caps
  /// it below the daemon's negotiated peer timeout so this deliberate shutdown
  /// happens before a blocked reader can masquerade as peer silence.
  pub presentation_backpressure_timeout: Duration,
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
      presentation_backpressure_timeout: Duration::from_secs(30),
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
  Detach,
}

/// Confirms that a renderer has applied one ordered presentation event.
///
/// Acknowledgements are local controller messages, never daemon protocol
/// frames. They determine the safe raw-output resume cursor after a reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresentationAck {
  Output { sequence_end: u64 },
  Checkpoint { sequence: u64 },
  CheckpointIncompatible { sequence: u64 },
  Geometry { observed_sequence: u64 },
  GeometryIncompatible { observed_sequence: u64 },
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

/// Failure to queue a renderer acknowledgement locally.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AttachmentAcknowledgementError {
  #[error("attachment controller is no longer running")]
  Closed,
}

/// Cloneable command endpoint for an [`AttachmentController`].
///
/// A successful method call only means the request entered the local ordered
/// queue. The daemon remains authoritative; a race with lease loss is reported
/// later as [`AttachmentEvent::ServerError`].
#[derive(Debug, Clone)]
pub struct AttachmentControl {
  commands: mpsc::Sender<AttachmentCommand>,
  acknowledgements: mpsc::Sender<PresentationAck>,
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

  /// Acknowledges that the renderer applied an `output` event through this
  /// exact sequence end.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn acknowledge_output(
    &self,
    sequence_end: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledgements
      .send(PresentationAck::Output { sequence_end })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)
  }

  /// Acknowledges that the renderer reset/applied a `checkpoint` event.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn acknowledge_checkpoint(
    &self,
    sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledgements
      .send(PresentationAck::Checkpoint { sequence })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)
  }

  /// Records a checkpoint that was displayed but not applied to a compatible
  /// terminal grid.
  ///
  /// Later output acknowledgements remain bookkeeping only; reconnect will
  /// keep omitting `resume_from` until a compatible checkpoint is applied.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn acknowledge_checkpoint_incompatible(
    &self,
    sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledgements
      .send(PresentationAck::CheckpointIncompatible { sequence })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)
  }

  /// Acknowledges that the renderer applied an ordered PTY geometry change.
  ///
  /// Geometry does not advance the raw resume cursor, but acknowledgement
  /// prevents a later output acknowledgement from overtaking the resize.
  ///
  /// # Errors
  ///
  /// Returns an error when the controller has already stopped.
  pub async fn acknowledge_geometry(
    &self,
    observed_sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledgements
      .send(PresentationAck::Geometry { observed_sequence })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)
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
  /// Returns an error when the controller has already stopped.
  pub async fn acknowledge_geometry_incompatible(
    &self,
    observed_sequence: u64,
  ) -> Result<(), AttachmentAcknowledgementError> {
    self
      .acknowledgements
      .send(PresentationAck::GeometryIncompatible { observed_sequence })
      .await
      .map_err(|_error| AttachmentAcknowledgementError::Closed)
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
  acknowledgements: Option<mpsc::Receiver<PresentationAck>>,
  events: mpsc::Sender<AttachmentEvent>,
  initial_checkpoint: Option<(TerminalCheckpoint, bool)>,
  /// A renderer may continue to present raw bytes after declaring a geometry
  /// incompatible, but it cannot resume safely until a checkpoint is applied.
  renderer_requires_checkpoint: bool,
  pending_presentations: VecDeque<PendingPresentation>,
}

/// Performs the versioned `rmux` handshake over any bidirectional stream.
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
      heartbeat_interval_ms,
      attachment_liveness_timeout_ms,
      ..
    } if protocol_version == PROTOCOL_VERSION => {
      attachment_liveness(heartbeat_interval_ms, attachment_liveness_timeout_ms).map(
        |attachment_liveness| HandshakeInfo {
          attachment_liveness,
        },
      )
    }
    response => Err(unexpected("handshake_accepted", &response)),
  }
}

/// Sends a single non-attachment request over any supported transport.
///
/// The stream is consumed because one-shot `rmux` requests complete after the
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
  mut stream: S,
  identity: &ClientIdentity,
  request: AttachRequest,
) -> Result<(S, AttachedSession), ClientError>
where
  S: AsyncRead + AsyncWrite + Unpin,
{
  let handshake = handshake(&mut stream, identity).await?;
  write_frame(
    &mut stream,
    &ClientMessage::AttachSession {
      session: request.session,
      resume_from: request.resume_from,
      terminal_size: request.terminal_size,
      request_input_lease: request.request_input_lease,
      request_layout_lease: request.request_layout_lease,
      request_command_line: request.request_command_line,
    },
  )
  .await?;

  let response = read_response(&mut stream).await?;
  let ServerMessage::Attached {
    session,
    replay_from,
    history_gap,
    checkpoint,
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
      session,
      replay_from,
      history_gap,
      checkpoint,
      terminal_size_mismatch,
      input_lease,
      layout_lease,
      shell_state_cache: ShellStateCache::new(shell_state.clone()),
      shell_state,
      liveness: handshake.attachment_liveness,
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
    mut options: AttachmentControllerOptions,
  ) -> Result<(Self, AttachmentControl, AttachmentEvents), ClientError> {
    if options.command_queue_capacity == 0 || options.event_queue_capacity == 0 {
      return Err(ClientError::InvalidAttachmentQueueCapacity {
        command_queue_capacity: options.command_queue_capacity,
        event_queue_capacity: options.event_queue_capacity,
      });
    }
    if options.presentation_backpressure_timeout.is_zero() {
      return Err(ClientError::InvalidPresentationBackpressureTimeout);
    }
    let latest_safe_backpressure_timeout = attached
      .liveness
      .peer_timeout
      .checked_div(2)
      .unwrap_or(attached.liveness.peer_timeout);
    options.presentation_backpressure_timeout = options
      .presentation_backpressure_timeout
      .min(latest_safe_backpressure_timeout);

    if let Some(checkpoint) = attached.checkpoint.as_ref() {
      validate_checkpoint(checkpoint)?;
      if checkpoint.sequence != attached.replay_from {
        return Err(ClientError::InvalidInitialCheckpointSequence {
          checkpoint_sequence: checkpoint.sequence,
          replay_from: attached.replay_from,
        });
      }
    }

    let state = AttachmentState::from_attached(attached);
    let (command_sender, commands) = mpsc::channel(options.command_queue_capacity);
    let (acknowledgement_sender, acknowledgements) = mpsc::channel(options.event_queue_capacity);
    let (events, event_receiver) = mpsc::channel(options.event_queue_capacity);
    let initial_checkpoint = attached
      .checkpoint
      .as_ref()
      .map(|checkpoint| (checkpoint.clone(), attached.history_gap));
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
    let peer_activity = Arc::new(Mutex::new(Instant::now()));
    let reader = read_server_messages(reader, incoming_sender, Arc::clone(&peer_activity));
    let writer = drive_attachment_writer(
      writer,
      commands,
      self.state.clone(),
      self.liveness,
      self.options.clone(),
      peer_activity,
      writer_status_sender,
    );
    let driver = self.drive(incoming_receiver, acknowledgements, writer_status_receiver);
    tokio::pin!(reader);
    tokio::pin!(writer);
    tokio::pin!(driver);

    tokio::select! {
      result = &mut driver => result,
      () = &mut reader => driver.await,
      () = &mut writer => driver.await,
    }
  }

  async fn drive(
    &mut self,
    mut incoming: mpsc::Receiver<IncomingServerMessage>,
    mut acknowledgements: mpsc::Receiver<PresentationAck>,
    mut writer_statuses: mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<AttachExit, ClientError> {
    if let Some((checkpoint, history_gap)) = self.initial_checkpoint.take() {
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
      tokio::select! {
        writer_status = writer_statuses.recv() => {
          return match writer_status {
            Some(WriterStatus::Detached) => Ok(self.finish(AttachExitReason::Detached)),
            Some(WriterStatus::ConnectionClosed) | None => {
              Ok(self.finish(AttachExitReason::ConnectionClosed))
            }
            Some(WriterStatus::Fatal(error)) => Err(error),
          };
        }
        acknowledgement = acknowledgements.recv(), if acknowledgements_open => {
          match acknowledgement {
            Some(acknowledgement) => self.accept_presentation_acknowledgement(acknowledgement)?,
            None => acknowledgements_open = false,
          }
        }
        incoming_message = incoming.recv() => {
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
        history_gap,
      } => {
        self
          .process_checkpoint(checkpoint, history_gap, writer_statuses)
          .await
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
        "output, checkpoint, pty_geometry_changed, shell_state_changed, lease_status, heartbeat_ack, or session_ended",
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
    if !self.enqueue_presentation(PendingPresentation::Output { sequence_end }) {
      return Ok(ControllerAction::Exit {
        reason: AttachExitReason::ConnectionClosed,
      });
    }
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
    history_gap: bool,
    writer_statuses: &mut mpsc::UnboundedReceiver<WriterStatus>,
  ) -> Result<ControllerAction, ClientError> {
    self.accept_checkpoint(&checkpoint)?;
    if !self.enqueue_presentation(PendingPresentation::Checkpoint {
      sequence: checkpoint.sequence,
    }) {
      return Ok(ControllerAction::Exit {
        reason: AttachExitReason::ConnectionClosed,
      });
    }
    self
      .emit_event(
        AttachmentEvent::Checkpoint {
          checkpoint,
          history_gap,
        },
        writer_statuses,
      )
      .await
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

  fn accept_checkpoint(&mut self, checkpoint: &TerminalCheckpoint) -> Result<(), ClientError> {
    validate_checkpoint(checkpoint)?;
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
    let expected_sequence = self.state.received_sequence();
    if observed_sequence != expected_sequence {
      return Err(ClientError::GeometryAheadOfOutput {
        expected_sequence,
        observed_sequence,
      });
    }

    self.state.set_terminal_size(terminal_size.clone());
    if !self.enqueue_presentation(PendingPresentation::Geometry { observed_sequence }) {
      return Ok(ControllerAction::Exit {
        reason: AttachExitReason::ConnectionClosed,
      });
    }
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
    // The extra slot permits one current event to wait in the bounded bridge
    // while the previous event is still awaiting an acknowledgement. Once a
    // presenter drains events without acknowledging them, this ledger remains
    // bounded and the controller closes for a checkpoint-safe reconnect.
    let capacity = self.options.event_queue_capacity.saturating_add(1);
    if self.pending_presentations.len() >= capacity {
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
    let backpressure = sleep(self.options.presentation_backpressure_timeout);
    tokio::pin!(send);
    tokio::pin!(backpressure);
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
      writer_status = writer_statuses.recv() => {
        match writer_status {
          Some(WriterStatus::Detached) => Ok(ControllerAction::Exit {
            reason: AttachExitReason::Detached,
          }),
          Some(WriterStatus::ConnectionClosed) | None => Ok(ControllerAction::Exit {
            reason: AttachExitReason::ConnectionClosed,
          }),
          Some(WriterStatus::Fatal(error)) => Err(error),
        }
      }
      () = &mut backpressure => {
        Ok(ControllerAction::Exit {
          reason: AttachExitReason::ConnectionClosed,
        })
      }
    }
  }

  fn accept_presentation_acknowledgement(
    &mut self,
    acknowledgement: PresentationAck,
  ) -> Result<(), ClientError> {
    let pending = self.pending_presentations.front().copied().ok_or_else(|| {
      ClientError::UnexpectedPresentationAcknowledgement {
        expected: "no pending presentation event".into(),
        actual: presentation_acknowledgement_name(acknowledgement).into(),
      }
    })?;
    let (acknowledged_sequence, applied_checkpoint, incompatible_checkpoint, incompatible_geometry) =
      match (pending, acknowledgement) {
        (
          PendingPresentation::Output {
            sequence_end: expected_sequence_end,
          },
          PresentationAck::Output { sequence_end },
        ) if sequence_end == expected_sequence_end => (Some(sequence_end), false, false, false),
        (
          PendingPresentation::Checkpoint {
            sequence: expected_sequence,
          },
          PresentationAck::Checkpoint { sequence },
        ) if sequence == expected_sequence => (Some(sequence), true, false, false),
        (
          PendingPresentation::Checkpoint {
            sequence: expected_sequence,
          },
          PresentationAck::CheckpointIncompatible { sequence },
        ) if sequence == expected_sequence => (None, false, true, false),
        (
          PendingPresentation::Geometry {
            observed_sequence: expected_sequence,
          },
          PresentationAck::Geometry { observed_sequence },
        ) if observed_sequence == expected_sequence => {
          (self.state.resume_sequence(), false, false, false)
        }
        (
          PendingPresentation::Geometry {
            observed_sequence: expected_sequence,
          },
          PresentationAck::GeometryIncompatible { observed_sequence },
        ) if observed_sequence == expected_sequence => (None, false, false, true),
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
    Ok(())
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
  Detached,
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

async fn drive_attachment_writer<W>(
  mut writer: W,
  mut commands: mpsc::Receiver<AttachmentCommand>,
  state: AttachmentState,
  liveness: AttachmentLiveness,
  options: AttachmentControllerOptions,
  peer_activity: Arc<Mutex<Instant>>,
  statuses: mpsc::UnboundedSender<WriterStatus>,
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
        if peer_is_silent(&peer_activity, liveness.peer_timeout) {
          break WriterStatus::ConnectionClosed;
        }
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
      command = commands.recv() => {
        match command {
          Some(AttachmentCommand::Detach) | None => {
            match send_attachment_message(&mut writer, &ClientMessage::Detach).await {
              Ok(true) => break WriterStatus::Detached,
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
    }
  };
  let _ignored = statuses.send(status);
}

fn peer_is_silent(peer_activity: &Arc<Mutex<Instant>>, peer_timeout: Duration) -> bool {
  let last_activity = match peer_activity.lock() {
    Ok(activity) => *activity,
    Err(poisoned) => *poisoned.into_inner(),
  };
  Instant::now().saturating_duration_since(last_activity) >= peer_timeout
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
/// checkpoint stream so the `rmux_vt_state` initialization program never
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
      "rmux: older scrollback is no longer retained; restoring sequence {}",
      attached.replay_from
    );
  }
  if attached.terminal_size_mismatch {
    let terminal_size = current_terminal_size();
    eprintln!(
      "rmux: terminal is {}x{}, but this session is {}x{}; the PTY will not be resized",
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
    eprintln!("rmux: view-only attachment ({reason})");
  }
  if !attached.layout_lease.owned_by_client && attached.layout_lease.held {
    eprintln!("rmux: another attachment owns PTY layout");
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
      | AttachmentEvent::HeartbeatAck { .. }
      | AttachmentEvent::Exited { .. } => {}
      AttachmentEvent::ServerError { message, .. } => eprintln!("\r\n[rmux: {message}]"),
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
  #[error("attachment controller presentation backpressure timeout must be non-zero")]
  InvalidPresentationBackpressureTimeout,
  #[error("attachment controller has already been started")]
  AttachmentControllerAlreadyRun,
  #[error("daemon error {code:?}: {message}")]
  Server { code: ErrorCode, message: String },
  #[error(transparent)]
  AttachmentCommand(#[from] AttachmentCommandError),
  #[error("unsupported terminal checkpoint format {format} version {format_version}")]
  UnsupportedCheckpoint { format: String, format_version: u16 },
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
          ..
        }
      ));
      write_frame(
        &mut daemon,
        &ServerMessage::Attached {
          session: session_info(),
          earliest_sequence: 0,
          next_sequence: 0,
          replay_from: 0,
          history_gap: false,
          checkpoint: None,
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
      },
    )
    .await
    .unwrap();

    assert_eq!(attached.session.name, "work");
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
    wait_for_resume_sequence(&state, Some(3)).await;

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(3));
    assert_eq!(exit.received_sequence, 3);
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
    wait_for_resume_sequence(&state, Some(3)).await;

    let checkpoint = checkpoint(10);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history_gap: true,
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint {
        checkpoint: checkpoint.clone(),
        history_gap: true,
      })
    );
    assert_eq!(state.received_sequence(), 10);
    assert_eq!(state.resume_sequence(), None);

    control
      .acknowledge_checkpoint(checkpoint.sequence)
      .await
      .unwrap();
    wait_for_resume_sequence(&state, Some(10)).await;

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
        history_gap: true,
      },
    )
    .await
    .unwrap();
    assert_eq!(
      events.recv().await,
      Some(AttachmentEvent::Checkpoint {
        checkpoint: checkpoint.clone(),
        history_gap: true,
      })
    );
    control
      .acknowledge_checkpoint(checkpoint.sequence)
      .await
      .unwrap();
    wait_for_resume_sequence(&state, Some(checkpoint.sequence)).await;

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
    wait_for_resume_sequence(&state, Some(checkpoint.sequence)).await;

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
    wait_for_resume_sequence(&state, Some(0)).await;

    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output after geometry event");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    wait_for_resume_sequence(&state, Some(3)).await;

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
    wait_for_resume_sequence(&state, None).await;

    let Some(AttachmentEvent::Output { sequence_end, .. }) = events.recv().await else {
      panic!("expected output after incompatible geometry");
    };
    control.acknowledge_output(sequence_end).await.unwrap();
    wait_for_resume_sequence(&state, None).await;

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
    wait_for_resume_sequence(&state, None).await;

    let checkpoint = checkpoint(3);
    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history_gap: false,
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
    wait_for_resume_sequence(&state, None).await;

    write_frame(
      &mut daemon,
      &ServerMessage::Checkpoint {
        checkpoint: checkpoint.clone(),
        history_gap: false,
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
    wait_for_resume_sequence(&state, Some(checkpoint.sequence)).await;

    drop(daemon);
    let exit = runner.await.unwrap().unwrap();
    assert_eq!(exit.next_sequence, Some(checkpoint.sequence));
  }

  #[tokio::test]
  async fn acknowledgement_ledger_is_bounded_when_events_are_not_acknowledged() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let attached = attached_session(0, None, ShellState::default());
    let options = AttachmentControllerOptions {
      event_queue_capacity: 1,
      ..controller_options()
    };
    let (controller, _control, mut events) =
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
      assert!(matches!(
        events.recv().await,
        Some(AttachmentEvent::Output { .. })
      ));
    }

    write_frame(
      &mut daemon,
      &ServerMessage::Output {
        sequence_start: 2,
        sequence_end: 3,
        data: b"c".to_vec(),
      },
    )
    .await
    .unwrap();

    let exit = tokio::time::timeout(Duration::from_secs(1), runner)
      .await
      .expect("unacknowledged presentation ledger did not close")
      .unwrap()
      .unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(0));
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
  async fn full_presentation_queue_keeps_heartbeats_live_then_reconnects_from_applied_cursor() {
    let (client, mut daemon) = tokio::io::duplex(4096);
    let mut attached = attached_session(0, None, ShellState::default());
    attached.liveness = AttachmentLiveness {
      heartbeat_interval: Duration::from_millis(10),
      peer_timeout: Duration::from_millis(100),
    };
    let options = AttachmentControllerOptions {
      event_queue_capacity: 1,
      presentation_backpressure_timeout: Duration::from_millis(30),
      ..controller_options()
    };
    let (controller, _control, _events) =
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

    let exit = tokio::time::timeout(Duration::from_millis(200), runner)
      .await
      .expect("full presentation queue did not trigger bounded shutdown")
      .unwrap()
      .unwrap();
    assert_eq!(exit.reason, AttachExitReason::ConnectionClosed);
    assert_eq!(exit.next_sequence, Some(0));
    assert_eq!(exit.received_sequence, 6);
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
    AttachmentControllerOptions {
      presentation_backpressure_timeout: Duration::from_secs(1),
      ..AttachmentControllerOptions::default()
    }
  }

  fn attached_session(
    replay_from: u64,
    checkpoint: Option<TerminalCheckpoint>,
    shell_state: ShellState,
  ) -> AttachedSession {
    AttachedSession {
      session: session_info(),
      replay_from,
      history_gap: false,
      checkpoint,
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
      format: rmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: rmux_proto::TERMINAL_CHECKPOINT_FORMAT_VERSION,
      sequence,
      terminal_size: TerminalSize::default(),
      payload: Vec::new(),
      input_prefix: Vec::new(),
    }
  }

  async fn wait_for_resume_sequence(state: &AttachmentState, expected: Option<u64>) {
    tokio::time::timeout(Duration::from_secs(1), async {
      loop {
        if state.resume_sequence() == expected {
          return;
        }
        tokio::task::yield_now().await;
      }
    })
    .await
    .expect("controller did not process renderer acknowledgement");
  }

  fn session_info() -> SessionInfo {
    SessionInfo {
      session_id: "session-id".into(),
      name: "work".into(),
      status: rmux_proto::SessionStatus::Running,
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
