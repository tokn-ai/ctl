use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 4;
pub const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;
pub const TERMINAL_CHECKPOINT_FORMAT: &str = "rmux_vt_state";
pub const TERMINAL_CHECKPOINT_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
  pub columns: u16,
  pub rows: u16,
  pub pixel_width: u16,
  pub pixel_height: u16,
}

impl Default for TerminalSize {
  fn default() -> Self {
    Self {
      columns: 80,
      rows: 24,
      pixel_width: 0,
      pixel_height: 0,
    }
  }
}

/// An independently controlled capability of an attached terminal client.
///
/// Input and PTY layout intentionally have separate owners. For example, a
/// phone may view and type into a desktop-sized session without changing its
/// layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
  Input,
  Layout,
}

/// Lease state as observed by one attached client.
///
/// The daemon deliberately does not expose another attachment's identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseStatus {
  pub held: bool,
  pub owned_by_client: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCheckpoint {
  pub format: String,
  pub format_version: u16,
  pub sequence: u64,
  pub terminal_size: TerminalSize,
  pub payload: Vec<u8>,
  pub input_prefix: Vec<u8>,
}

impl TerminalCheckpoint {
  #[must_use]
  pub fn is_supported(&self) -> bool {
    self.format == TERMINAL_CHECKPOINT_FORMAT
      && self.format_version == TERMINAL_CHECKPOINT_FORMAT_VERSION
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandSpec {
  pub program: String,
  pub arguments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
  Running,
  Exited,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
  pub session_id: String,
  pub name: String,
  pub status: SessionStatus,
  pub created_at_ms: u64,
  pub next_sequence: u64,
  pub terminal_size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  Handshake {
    protocol_version: u16,
    client_name: String,
    client_version: String,
  },
  CreateSession {
    name: Option<String>,
    command: Option<CommandSpec>,
    working_directory: Option<String>,
    terminal_size: TerminalSize,
  },
  ListSessions,
  AttachSession {
    session: String,
    resume_from: Option<u64>,
    terminal_size: TerminalSize,
    /// Claim input if no other attached client currently owns it. This never
    /// takes the lease from another client.
    request_input_lease: bool,
    /// Claim PTY layout ownership if unheld. A successful request applies the
    /// terminal size in this attach request as an explicit resize.
    request_layout_lease: bool,
  },
  KillSession {
    session: String,
  },
  Input {
    data: Vec<u8>,
  },
  Resize {
    terminal_size: TerminalSize,
  },
  AcquireLease {
    lease: LeaseKind,
  },
  ReleaseLease {
    lease: LeaseKind,
  },
  /// Confirms that an attached client is still reachable without affecting
  /// terminal input or layout ownership.
  Heartbeat {
    nonce: u64,
  },
  Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
  InvalidRequest,
  InvalidSessionName,
  ProtocolVersionMismatch,
  SequenceAhead,
  SessionAlreadyExists,
  SessionNotFound,
  InputLeaseRequired,
  LayoutLeaseRequired,
  Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted {
    protocol_version: u16,
    server_version: String,
    /// Suggested cadence for attached-client heartbeats.
    heartbeat_interval_ms: u64,
    /// Maximum interval without client activity before an attachment expires.
    attachment_liveness_timeout_ms: u64,
  },
  SessionCreated {
    session: SessionInfo,
  },
  SessionList {
    sessions: Vec<SessionInfo>,
  },
  Attached {
    session: SessionInfo,
    earliest_sequence: u64,
    next_sequence: u64,
    replay_from: u64,
    history_gap: bool,
    checkpoint: Option<TerminalCheckpoint>,
    terminal_size_mismatch: bool,
    input_lease: LeaseStatus,
    layout_lease: LeaseStatus,
  },
  LeaseStatus {
    lease: LeaseKind,
    status: LeaseStatus,
  },
  /// Echoes an attached client's heartbeat nonce.
  HeartbeatAck {
    nonce: u64,
  },
  Checkpoint {
    checkpoint: TerminalCheckpoint,
    history_gap: bool,
  },
  Output {
    sequence_start: u64,
    sequence_end: u64,
    data: Vec<u8>,
  },
  SessionEnded {
    session_id: String,
    exit_code: Option<u32>,
  },
  Success,
  Error {
    code: ErrorCode,
    message: String,
  },
}

#[derive(Debug, Error)]
pub enum CodecError {
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("frame length {actual} exceeds the maximum of {maximum} bytes")]
  FrameTooLarge { actual: usize, maximum: usize },
  #[error("invalid JSON frame: {0}")]
  Json(#[from] serde_json::Error),
}

/// Serializes and writes one length-prefixed protocol frame.
///
/// # Errors
///
/// Returns an error when serialization fails, the encoded message is too
/// large, or the transport cannot be written or flushed.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), CodecError>
where
  W: AsyncWrite + Unpin,
  T: Serialize,
{
  let payload = serde_json::to_vec(message)?;
  if payload.len() > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: payload.len(),
      maximum: MAX_FRAME_SIZE,
    });
  }

  #[allow(clippy::cast_possible_truncation)]
  let length = payload.len() as u32;
  writer.write_all(&length.to_be_bytes()).await?;
  writer.write_all(&payload).await?;
  writer.flush().await?;
  Ok(())
}

/// Reads and deserializes one length-prefixed protocol frame.
///
/// A clean end of stream before a new frame returns `Ok(None)`.
///
/// # Errors
///
/// Returns an error when the transport fails mid-frame, the declared frame is
/// too large, or its payload is not valid JSON for the requested type.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, CodecError>
where
  R: AsyncRead + Unpin,
  T: DeserializeOwned,
{
  let mut length_bytes = [0_u8; 4];
  match reader.read(&mut length_bytes[..1]).await {
    Ok(0) => return Ok(None),
    Ok(_) => {
      reader.read_exact(&mut length_bytes[1..]).await?;
    }
    Err(error) => return Err(error.into()),
  }

  let length = u32::from_be_bytes(length_bytes) as usize;
  if length > MAX_FRAME_SIZE {
    return Err(CodecError::FrameTooLarge {
      actual: length,
      maximum: MAX_FRAME_SIZE,
    });
  }

  let mut payload = vec![0_u8; length];
  reader.read_exact(&mut payload).await?;
  Ok(Some(serde_json::from_slice(&payload)?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn frames_round_trip() {
    let expected = ClientMessage::AttachSession {
      session: "work".into(),
      resume_from: Some(42),
      terminal_size: TerminalSize::default(),
      request_input_lease: true,
      request_layout_lease: false,
    };
    let (mut client, mut server) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut client, &expected).await });
    let actual: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ClientMessage::AttachSession {
        session: "work".into(),
        resume_from: Some(42),
        terminal_size: TerminalSize::default(),
        request_input_lease: true,
        request_layout_lease: false,
      }
    );
  }

  #[tokio::test]
  async fn lease_status_frame_round_trips() {
    let expected = ServerMessage::LeaseStatus {
      lease: LeaseKind::Layout,
      status: LeaseStatus {
        held: true,
        owned_by_client: false,
      },
    };
    let (mut server, mut client) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut server, &expected).await });
    let actual: ServerMessage = read_frame(&mut client).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(
      actual,
      ServerMessage::LeaseStatus {
        lease: LeaseKind::Layout,
        status: LeaseStatus {
          held: true,
          owned_by_client: false,
        },
      }
    );
  }

  #[tokio::test]
  async fn heartbeat_frames_round_trip() {
    let expected = ClientMessage::Heartbeat { nonce: 42 };
    let (mut client, mut server) = tokio::io::duplex(1024);

    let write = tokio::spawn(async move { write_frame(&mut client, &expected).await });
    let actual: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(actual, ClientMessage::Heartbeat { nonce: 42 });
  }

  #[tokio::test]
  async fn clean_eof_returns_none() {
    let (client, mut server) = tokio::io::duplex(16);
    drop(client);

    let message: Option<ClientMessage> = read_frame(&mut server).await.unwrap();
    assert!(message.is_none());
  }

  #[tokio::test]
  async fn truncated_header_is_an_error() {
    let (mut client, mut server) = tokio::io::duplex(16);
    client.write_all(&[0, 0]).await.unwrap();
    drop(client);

    let result = read_frame::<_, ClientMessage>(&mut server).await;
    assert!(matches!(
      result,
      Err(CodecError::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof
    ));
  }
}
