//! Fixed, confirmation-bound maintenance over an already authenticated SSH channel.
use ctl_core::component::{ComponentBuildInfo, ComponentInfo};
use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const PROTOCOL_BUILD: u16 = 2;
pub const CONTRACT_V1_0_2: ProtocolVersion = ProtocolVersion::new(1, 0, 2);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_2;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_2];

#[must_use]
pub fn protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}
const MAX_FRAME_BYTES: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
  PrepareCtmuxRestart {
    protocol: ProtocolOffer,
    expected_remote_id: String,
  },
  Confirm {},
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningCtmux {
  pub build: Option<ComponentBuildInfo>,
  pub protocol_version: Option<ProtocolVersion>,
  pub control_protocol_version: ProtocolVersion,
  pub protocols: Vec<ctl_core::component::ProtocolInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtmuxPreparation {
  pub remote_id: String,
  pub running: RunningCtmux,
  pub available: ComponentInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CtmuxRestartCompleted {
  pub after: ComponentInfo,
  pub terminated_sessions: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServerMessage {
  Prepared {
    protocol_version: ProtocolVersion,
    info: CtmuxPreparation,
  },
  Completed {
    result: CtmuxRestartCompleted,
  },
  Error {
    code: String,
    message: String,
    may_have_stopped: bool,
  },
}

/// Reads one bounded maintenance message without consuming subsequent messages.
///
/// # Errors
/// Returns an error for a closed channel, oversized frame, or invalid JSON.
pub async fn read<R: AsyncRead + Unpin, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
  let size = reader.read_u32().await? as usize;
  if size == 0 || size > MAX_FRAME_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Invalid maintenance frame size.",
    ));
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  serde_json::from_slice(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes and flushes one bounded maintenance message.
///
/// # Errors
/// Returns encoding, frame-size, or channel I/O errors.
pub async fn write<W: AsyncWrite + Unpin, T: Serialize>(
  writer: &mut W,
  value: &T,
) -> io::Result<()> {
  let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
  if bytes.len() > MAX_FRAME_BYTES {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "Maintenance frame is too large.",
    ));
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(io::Error::other)?)
    .await?;
  writer.write_all(&bytes).await?;
  writer.flush().await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn rejects_unbounded_frames_and_arbitrary_operations() {
    assert!(
      read::<_, ClientMessage>(&mut u32::MAX.to_be_bytes().as_slice())
        .await
        .is_err()
    );
    assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"confirm","command":"sh"}"#).is_err());
    assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"exec"}"#).is_err());
  }

  #[tokio::test]
  async fn preparation_and_confirmation_are_separate_frames() {
    let mut bytes = Vec::new();
    write(
      &mut bytes,
      &ClientMessage::PrepareCtmuxRestart {
        protocol: protocol_offer(),
        expected_remote_id: "test-identity".into(),
      },
    )
    .await
    .unwrap();
    write(&mut bytes, &ClientMessage::Confirm {}).await.unwrap();
    let mut reader = bytes.as_slice();
    assert!(matches!(
      read::<_, ClientMessage>(&mut reader).await.unwrap(),
      ClientMessage::PrepareCtmuxRestart { .. }
    ));
    assert_ne!(reader, &[] as &[u8]);
    assert!(matches!(
      read::<_, ClientMessage>(&mut reader).await.unwrap(),
      ClientMessage::Confirm {}
    ));
  }
}
