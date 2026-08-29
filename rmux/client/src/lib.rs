use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size};
use rmux_proto::{
  ClientMessage, CodecError, ErrorCode, LeaseKind, LeaseStatus, PROTOCOL_VERSION, ServerMessage,
  SessionInfo, TerminalCheckpoint, TerminalSize, read_frame, write_frame,
};
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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
/// Returns an error when the transport fails, the daemon rejects the request,
/// or the daemon replies with an unexpected protocol message.
pub async fn handshake<S>(stream: &mut S, identity: &ClientIdentity) -> Result<(), ClientError>
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
      protocol_version, ..
    } if protocol_version == PROTOCOL_VERSION => Ok(()),
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
  handshake(&mut stream, identity).await?;
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
  report_attachment(attached);
  let interactive = io::stdin().is_terminal();
  let _raw_mode = RawModeGuard::enable_if(interactive)?;
  if let Some(checkpoint) = attached.checkpoint.as_ref() {
    let mut stdout = tokio::io::stdout();
    restore_checkpoint(&mut stdout, checkpoint).await?;
  }

  let last_sequence = Arc::new(AtomicU64::new(attached.replay_from));
  let (socket_reader, socket_writer) = tokio::io::split(stream);
  let reason = tokio::select! {
    result = forward_input(socket_writer, attached.input_lease.owned_by_client) => result?,
    result = forward_output(socket_reader, Arc::clone(&last_sequence)) => result?,
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
  input_enabled: bool,
) -> Result<AttachExitReason, ClientError>
where
  W: AsyncWrite + Unpin,
{
  let mut stdin = tokio::io::stdin();
  let mut buffer = vec![0_u8; 4096];
  let mut reported_view_only = false;
  loop {
    let bytes_read = stdin.read(&mut buffer).await?;
    if bytes_read == 0 {
      return if send_attachment_message(&mut socket_writer, &ClientMessage::Detach).await? {
        Ok(AttachExitReason::Detached)
      } else {
        Ok(AttachExitReason::ConnectionClosed)
      };
    }

    let input = &buffer[..bytes_read];
    if let Some(detach_at) = input.iter().position(|byte| *byte == DETACH_BYTE) {
      if input_enabled
        && detach_at > 0
        && !send_attachment_message(
          &mut socket_writer,
          &ClientMessage::Input {
            data: input[..detach_at].to_vec(),
          },
        )
        .await?
      {
        return Ok(AttachExitReason::ConnectionClosed);
      }
      return if send_attachment_message(&mut socket_writer, &ClientMessage::Detach).await? {
        Ok(AttachExitReason::Detached)
      } else {
        Ok(AttachExitReason::ConnectionClosed)
      };
    }

    if !input_enabled {
      if !reported_view_only {
        eprintln!("\r\n[view-only attachment; press Ctrl-] to detach]");
        reported_view_only = true;
      }
      continue;
    }

    if !send_attachment_message(
      &mut socket_writer,
      &ClientMessage::Input {
        data: input.to_vec(),
      },
    )
    .await?
    {
      return Ok(AttachExitReason::ConnectionClosed);
    }
  }
}

async fn forward_output<R>(
  mut socket_reader: R,
  last_sequence: Arc<AtomicU64>,
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
        let owner = if status.owned_by_client {
          "owned by this attachment"
        } else if status.held {
          "owned by another attachment"
        } else {
          "available"
        };
        eprintln!("\r\n[{} lease is {owner}]", lease_name(lease));
      }
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
          eprintln!("\r\n[rmux: {message}]");
          continue;
        }
        return Err(ClientError::Server { code, message });
      }
      response => {
        return Err(unexpected(
          "output, checkpoint, lease_status, or session_ended",
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
