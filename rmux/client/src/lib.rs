use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use rmux_proto::{
  ClientMessage, CodecError, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, TerminalCheckpoint, TerminalSize, read_frame, write_frame,
};
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};

const DETACH_BYTE: u8 = 0x1d;

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
  /// Server-negotiated attachment liveness settings.
  pub liveness: AttachmentLiveness,
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
  pub next_sequence: u64,
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
      liveness: handshake.attachment_liveness,
    },
  ))
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
  if let Some(checkpoint) = attached.checkpoint.as_ref() {
    let mut stdout = tokio::io::stdout();
    restore_checkpoint(&mut stdout, checkpoint).await?;
  }

  let last_sequence = Arc::new(AtomicU64::new(attached.replay_from));
  let leases = Arc::new(AttachmentLeaseState::new(
    attached.input_lease.owned_by_client,
    attached.layout_lease.owned_by_client,
  ));
  let (peer_activity_sender, peer_activity_receiver) = watch::channel(0_u64);
  let (socket_reader, socket_writer) = tokio::io::split(stream);
  let reason = tokio::select! {
    result = forward_input(
      socket_writer,
      Arc::clone(&leases),
      attached.liveness,
      options,
    ) => result?,
    result = forward_output(
      socket_reader,
      Arc::clone(&last_sequence),
      leases,
      peer_activity_sender,
    ) => result?,
    reason = detect_peer_silence(peer_activity_receiver, attached.liveness.peer_timeout) => reason,
  };

  Ok(AttachExit {
    reason,
    next_sequence: last_sequence.load(Ordering::Acquire),
  })
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

/// Restores a compatible terminal checkpoint to an asynchronous output.
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

async fn detect_peer_silence(
  mut peer_activity: watch::Receiver<u64>,
  peer_timeout: Duration,
) -> AttachExitReason {
  let mut deadline = Instant::now() + peer_timeout;
  loop {
    tokio::select! {
      biased;
      changed = peer_activity.changed() => {
        if changed.is_err() {
          return AttachExitReason::ConnectionClosed;
        }
        peer_activity.borrow_and_update();
        deadline = Instant::now() + peer_timeout;
      }
      () = sleep_until(deadline) => return AttachExitReason::ConnectionClosed,
    }
  }
}

fn report_attachment(attached: &AttachedSession) {
  if attached.history_gap {
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

async fn forward_input<W>(
  mut socket_writer: W,
  leases: Arc<AttachmentLeaseState>,
  liveness: AttachmentLiveness,
  options: InteractiveAttachOptions,
) -> Result<AttachExitReason, ClientError>
where
  W: AsyncWrite + Unpin,
{
  let mut stdin = tokio::io::stdin();
  let mut buffer = vec![0_u8; 4096];
  let mut reported_view_only = false;
  let now = Instant::now();
  let mut heartbeats = interval_at(
    now + liveness.heartbeat_interval,
    liveness.heartbeat_interval,
  );
  heartbeats.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut heartbeat_nonce = 0_u64;
  let mut resize_after_layout_reacquire = options.reacquire_layout_lease
    && options.resize_after_layout_reacquire
    && !leases.layout_owned();

  loop {
    tokio::select! {
      _ = heartbeats.tick() => {
        if !send_heartbeat_tick(
          &mut socket_writer,
          &leases,
          options,
          &mut resize_after_layout_reacquire,
          &mut heartbeat_nonce,
        )
        .await?
        {
          return Ok(AttachExitReason::ConnectionClosed);
        }
      }
      bytes_read = stdin.read(&mut buffer) => {
        let bytes_read = bytes_read?;
        if bytes_read == 0 {
          return detach(&mut socket_writer).await;
        }
        if let Some(reason) = process_terminal_input(
          &mut socket_writer,
          &buffer[..bytes_read],
          &leases,
          &mut reported_view_only,
        )
        .await?
        {
          return Ok(reason);
        }
      }
    }
  }
}

async fn send_heartbeat_tick<W>(
  writer: &mut W,
  leases: &AttachmentLeaseState,
  options: InteractiveAttachOptions,
  resize_after_layout_reacquire: &mut bool,
  heartbeat_nonce: &mut u64,
) -> Result<bool, ClientError>
where
  W: AsyncWrite + Unpin,
{
  if options.reacquire_input_lease
    && !leases.input_owned()
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
    && !leases.layout_owned()
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
  if *resize_after_layout_reacquire && leases.layout_owned() {
    if !send_attachment_message(
      writer,
      &ClientMessage::Resize {
        terminal_size: current_terminal_size(),
      },
    )
    .await?
    {
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

async fn process_terminal_input<W>(
  writer: &mut W,
  input: &[u8],
  leases: &AttachmentLeaseState,
  reported_view_only: &mut bool,
) -> Result<Option<AttachExitReason>, ClientError>
where
  W: AsyncWrite + Unpin,
{
  if let Some(detach_at) = input.iter().position(|byte| *byte == DETACH_BYTE) {
    if leases.input_owned()
      && detach_at > 0
      && !send_attachment_message(
        writer,
        &ClientMessage::Input {
          data: input[..detach_at].to_vec(),
        },
      )
      .await?
    {
      return Ok(Some(AttachExitReason::ConnectionClosed));
    }
    return detach(writer).await.map(Some);
  }

  if !leases.input_owned() {
    if !*reported_view_only {
      eprintln!("\r\n[view-only attachment; press Ctrl-] to detach]");
      *reported_view_only = true;
    }
    return Ok(None);
  }

  *reported_view_only = false;
  if send_attachment_message(
    writer,
    &ClientMessage::Input {
      data: input.to_vec(),
    },
  )
  .await?
  {
    Ok(None)
  } else {
    Ok(Some(AttachExitReason::ConnectionClosed))
  }
}

async fn detach<W>(writer: &mut W) -> Result<AttachExitReason, ClientError>
where
  W: AsyncWrite + Unpin,
{
  if send_attachment_message(writer, &ClientMessage::Detach).await? {
    Ok(AttachExitReason::Detached)
  } else {
    Ok(AttachExitReason::ConnectionClosed)
  }
}

async fn forward_output<R>(
  mut socket_reader: R,
  last_sequence: Arc<AtomicU64>,
  leases: Arc<AttachmentLeaseState>,
  peer_activity: watch::Sender<u64>,
) -> Result<AttachExitReason, ClientError>
where
  R: AsyncRead + Unpin,
{
  let mut stdout = tokio::io::stdout();
  loop {
    let message = match read_frame::<_, ServerMessage>(&mut socket_reader).await {
      Ok(Some(message)) => message,
      Ok(None) | Err(CodecError::Io(_)) => return Ok(AttachExitReason::ConnectionClosed),
      Err(error) => return Err(error.into()),
    };
    peer_activity.send_modify(|activity| *activity = activity.wrapping_add(1));
    match message {
      ServerMessage::Output {
        sequence_end, data, ..
      } => {
        stdout.write_all(&data).await?;
        stdout.flush().await?;
        last_sequence.store(sequence_end, Ordering::Release);
      }
      ServerMessage::Checkpoint {
        checkpoint,
        history_gap,
      } => {
        if history_gap {
          eprintln!("\r\n[older scrollback is no longer retained; terminal state restored]");
        }
        restore_checkpoint(&mut stdout, &checkpoint).await?;
        last_sequence.store(checkpoint.sequence, Ordering::Release);
      }
      ServerMessage::LeaseStatus { lease, status } => {
        leases.set(lease, status.owned_by_client);
        let owner = if status.owned_by_client {
          "owned by this attachment"
        } else if status.held {
          "owned by another attachment"
        } else {
          "available"
        };
        eprintln!("\r\n[{} lease is {owner}]", lease_name(lease));
      }
      ServerMessage::HeartbeatAck { .. } => {}
      ServerMessage::SessionEnded { exit_code, .. } => {
        stdout.flush().await?;
        eprintln!("\r\n[session ended with exit code {exit_code:?}]");
        return Ok(AttachExitReason::SessionEnded { exit_code });
      }
      ServerMessage::Error { code, message } => {
        if matches!(
          code,
          ErrorCode::InputLeaseRequired | ErrorCode::LayoutLeaseRequired
        ) {
          match code {
            ErrorCode::InputLeaseRequired => leases.set(LeaseKind::Input, false),
            ErrorCode::LayoutLeaseRequired => leases.set(LeaseKind::Layout, false),
            _ => unreachable!("the match guard limits the error codes"),
          }
          eprintln!("\r\n[rmux: {message}]");
          continue;
        }
        return Err(ClientError::Server { code, message });
      }
      response => {
        return Err(unexpected(
          "output, checkpoint, lease_status, heartbeat_ack, or session_ended",
          &response,
        ));
      }
    }
  }
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

struct AttachmentLeaseState {
  input_owned: AtomicBool,
  layout_owned: AtomicBool,
}

impl AttachmentLeaseState {
  fn new(input_owned: bool, layout_owned: bool) -> Self {
    Self {
      input_owned: AtomicBool::new(input_owned),
      layout_owned: AtomicBool::new(layout_owned),
    }
  }

  fn input_owned(&self) -> bool {
    self.input_owned.load(Ordering::Acquire)
  }

  fn layout_owned(&self) -> bool {
    self.layout_owned.load(Ordering::Acquire)
  }

  fn set(&self, lease: LeaseKind, owned: bool) {
    match lease {
      LeaseKind::Input => self.input_owned.store(owned, Ordering::Release),
      LeaseKind::Layout => self.layout_owned.store(owned, Ordering::Release),
    }
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
  #[error("daemon error {code:?}: {message}")]
  Server { code: ErrorCode, message: String },
  #[error("unsupported terminal checkpoint format {format} version {format_version}")]
  UnsupportedCheckpoint { format: String, format_version: u16 },
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
      },
    )
    .await
    .unwrap();

    assert_eq!(attached.session.name, "work");
    assert!(attached.input_lease.owned_by_client);
    assert_eq!(attached.liveness.heartbeat_interval, Duration::from_secs(1));
    assert_eq!(attached.liveness.peer_timeout, Duration::from_secs(3));
    drop(stream);
    server.await.unwrap();
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
}
