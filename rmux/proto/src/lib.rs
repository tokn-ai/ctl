use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;

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
  Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted {
    protocol_version: u16,
    server_version: String,
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
      }
    );
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
