use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 2;
pub const MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
  Interactive,
  Background,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDefinition {
  pub name: String,
  pub program: String,
  pub arguments: Vec<String>,
  pub working_directory: Option<String>,
  pub execution_mode: ExecutionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredState {
  Stopped,
  Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
  Starting,
  Unknown,
  Running,
  Completed,
  Failed,
  Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveRun {
  #[serde(default)]
  pub released: bool,
  pub rmux_socket: std::path::PathBuf,
  pub instance_id: String,
  pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunInfo {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub interactive: Option<InteractiveRun>,
  pub run_id: String,
  pub state: RunState,
  pub started_at_ms: u64,
  pub ended_at_ms: Option<u64>,
  pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
  pub task_id: String,
  pub definition: TaskDefinition,
  pub desired_state: DesiredState,
  pub active_run: Option<RunInfo>,
  pub last_run: Option<RunInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
  Stdout,
  Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEvent {
  pub run_id: String,
  pub sequence: u64,
  pub stream: LogStream,
  pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  Handshake {
    protocol_version: u16,
    client_name: String,
  },
  CreateTask {
    definition: TaskDefinition,
  },
  ListTasks,
  ShowTask {
    task: String,
  },
  StartTask {
    task: String,
  },
  StopTask {
    task: String,
  },
  RestartTask {
    task: String,
  },
  RemoveTask {
    task: String,
  },
  ReadLogs {
    task: String,
    after_sequence: Option<u64>,
    follow: bool,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HandshakeAccepted { protocol_version: u16 },
  TaskCreated { task: TaskInfo },
  TaskList { tasks: Vec<TaskInfo> },
  TaskStatus { task: TaskInfo },
  TaskRemoved { task_id: String },
  Log { event: LogEvent },
  LogsFinished,
  Error { code: ErrorCode, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
  InvalidRequest,
  ProtocolVersionMismatch,
  InvalidDefinition,
  TaskNotFound,
  NameConflict,
  AlreadyRunning,
  NotRunning,
  UnsupportedExecutionMode,
  Internal,
}

#[derive(Debug, Error)]
pub enum CodecError {
  #[error("I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("frame length {actual} exceeds the maximum of {maximum} bytes")]
  FrameTooLarge { actual: usize, maximum: usize },
  #[error("invalid task protocol JSON frame: {0}")]
  Json(#[from] serde_json::Error),
}

/// Writes one length-prefixed task protocol message.
///
/// # Errors
///
/// Returns an error when serialization fails, the encoded frame is oversized,
/// or the transport cannot be written.
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
  let length = u32::try_from(payload.len()).map_err(|_| CodecError::FrameTooLarge {
    actual: payload.len(),
    maximum: MAX_FRAME_SIZE,
  })?;
  writer.write_all(&length.to_be_bytes()).await?;
  writer.write_all(&payload).await?;
  writer.flush().await?;
  Ok(())
}

/// Reads one length-prefixed task protocol message.
///
/// A clean end of stream before the next frame returns `Ok(None)`.
///
/// # Errors
///
/// Returns an error when the transport fails mid-frame, the declared frame is
/// oversized, or the payload cannot be decoded.
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
  let mut payload = vec![0; length];
  reader.read_exact(&mut payload).await?;
  Ok(Some(serde_json::from_slice(&payload)?))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn legacy_background_runs_remain_readable() {
    let run: RunInfo = serde_json::from_str(
      r#"{"run_id":"old-run","state":"completed","started_at_ms":1,"ended_at_ms":2,"exit_code":0}"#,
    )
    .unwrap();
    assert_eq!(run.state, RunState::Completed);
    assert!(run.interactive.is_none());
  }

  #[tokio::test]
  async fn frames_round_trip() {
    let message = ClientMessage::ListTasks;
    let (mut client, mut server) = tokio::io::duplex(1024);
    let write = write_frame(&mut client, &message);
    let read = read_frame::<_, ClientMessage>(&mut server);
    let (written, received) = tokio::join!(write, read);
    written.unwrap();
    assert_eq!(received.unwrap(), Some(message));
  }
}
