//! Fixed, confirmation-bound maintenance over an already authenticated SSH channel.
pub use ctl_core::component::LegacyProtocolInfo;
use ctl_core::component::{ComponentBuildInfo, ComponentInfo};
use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub const PROTOCOL_BUILD: u16 = 3;
pub const CONTRACT_V1_0_2: ProtocolVersion = ProtocolVersion::new(1, 0, 2);
pub const CONTRACT_V1_0_3: ProtocolVersion = ProtocolVersion::new(1, 0, 3);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_3;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_2, CONTRACT_V1_0_3];

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
  InspectComponents {
    protocol: ProtocolOffer,
    expected_remote_id: String,
  },
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
  pub control_protocol_version: Option<ProtocolVersion>,
  pub protocols: Vec<ctl_core::component::ProtocolInfo>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub legacy_protocols: Vec<LegacyProtocolInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
  CtlAgent,
  Ctld,
  Ctmuxd,
  CtlTaskd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
  Running,
  NotRunning,
  Legacy,
  Unavailable,
  OnDemand,
}

/// Recognizes the fixed historical control handshake without assigning it a
/// published contract. Numeric control 1 uses the cooperative restart request.
#[must_use]
pub fn valid_legacy_ctmux(protocols: &[LegacyProtocolInfo]) -> bool {
  (1..=2).contains(&protocols.len())
    && protocols
      .iter()
      .filter(|p| p.name == "ctmux_control" && p.version == 1)
      .count()
      == 1
    && protocols.iter().all(|p| {
      (p.name == "ctmux_control" && p.version == 1) || (p.name == "ctmux" && p.version > 0)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteComponent {
  pub component: ComponentKind,
  pub installed: Option<ComponentInfo>,
  pub running: Option<ComponentInfo>,
  pub state: ComponentState,
  pub restart_supported: bool,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub legacy_protocols: Vec<LegacyProtocolInfo>,
  pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteComponents {
  pub remote_id: String,
  pub components: Vec<RemoteComponent>,
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
  Components {
    protocol_version: ProtocolVersion,
    snapshot: RemoteComponents,
  },
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

  #[test]
  fn component_inspection_retains_the_published_restart_contract() {
    assert_eq!(
      protocol_offer().negotiate(&[CONTRACT_V1_0_2]),
      Some(CONTRACT_V1_0_2)
    );
    assert_eq!(
      protocol_offer().negotiate(&[CONTRACT_V1_0_3]),
      Some(CONTRACT_V1_0_3)
    );
    let running = RunningCtmux {
      build: None,
      protocol_version: None,
      control_protocol_version: Some(ProtocolVersion::new(1, 0, 1)),
      protocols: Vec::new(),
      legacy_protocols: Vec::new(),
    };
    let json = serde_json::to_value(&running).unwrap();
    assert_eq!(json["control_protocol_version"], "1.0.1");
    assert!(json.get("legacy_protocols").is_none());
  }

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
