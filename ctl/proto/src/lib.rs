//! Versioned metadata preceding the service protocol on identified SSH streams.
mod identity_contract;
pub mod maintenance;
use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
pub use identity_contract::{accept_identity_contract, negotiate_identity_contract};
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TcpListener {
  pub bind_address: String,
  pub port: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpListenerCatalog {
  pub listeners: Vec<TcpListener>,
  pub warnings: Vec<String>,
}

/// Confirmation-bound maintenance request, sent on stdin rather than through a shell.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCtmuxRestartRequest {
  pub expected_remote_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCtmuxRestartResult {
  pub terminated_sessions: u32,
}

pub const IDENTITY_PROTOCOL_BUILD: u16 = 3;
pub const IDENTITY_CONTRACT_V1_0_3: ProtocolVersion = ProtocolVersion::new(1, 0, 3);
pub const IDENTITY_PROTOCOL_VERSION: ProtocolVersion = IDENTITY_CONTRACT_V1_0_3;
pub const IDENTITY_SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[IDENTITY_CONTRACT_V1_0_3];
/// Stable framing marker. Compatibility is negotiated after this marker.
pub const IDENTITY_PREFACE: &[u8] = b"ctl-ssh-identity\n";
const MAX_IDENTITY_BYTES: usize = 8192;

#[must_use]
pub fn identity_protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    IDENTITY_PROTOCOL_BUILD,
    IDENTITY_PROTOCOL_VERSION,
    IDENTITY_SUPPORTED_PROTOCOL_VERSIONS,
  )
}

/// Protocol contracts implemented by this agent build, independent of release version.
#[must_use]
pub fn agent_protocols() -> Vec<ctl_core::component::ProtocolInfo> {
  vec![
    ctl_core::component::ProtocolInfo::new(
      "ctl_identity",
      IDENTITY_PROTOCOL_BUILD,
      IDENTITY_PROTOCOL_VERSION,
      IDENTITY_SUPPORTED_PROTOCOL_VERSIONS,
    ),
    ctl_core::component::ProtocolInfo::new(
      "ctl_maintenance",
      maintenance::PROTOCOL_BUILD,
      maintenance::PROTOCOL_VERSION,
      maintenance::SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ]
}

/// Stable identity of a remote user's ctl environment, independent of its address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteIdentity {
  pub remote_id: String,
  pub agent_version: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub build: Option<ctl_core::component::ComponentBuildInfo>,
  #[serde(default)]
  pub ctmux_restart_supported: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub bundle: Option<Box<BundleVersion>>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub protocols: Vec<ctl_core::component::ProtocolInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BundleVersion {
  pub app_version: String,
  pub bundle_id: String,
  pub git_revision: String,
  pub target_triple: String,
}

impl RemoteIdentity {
  #[must_use]
  pub fn is_valid(&self) -> bool {
    let text =
      |value: &str| !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control);
    uuid::Uuid::parse_str(&self.remote_id)
      .is_ok_and(|id| !id.is_nil() && id.to_string() == self.remote_id)
      && text(&self.agent_version)
      && self
        .build
        .as_ref()
        .is_none_or(|build| build.is_valid() && build.version == self.agent_version)
      && self.bundle.as_ref().is_none_or(|bundle| {
        [
          &bundle.app_version,
          &bundle.bundle_id,
          &bundle.git_revision,
          &bundle.target_triple,
        ]
        .into_iter()
        .all(|value| text(value))
      })
      && self.protocols.len() <= 32
      && self
        .protocols
        .iter()
        .all(ctl_core::component::ProtocolInfo::is_valid)
      && self.protocols.iter().enumerate().all(|(index, protocol)| {
        !self.protocols[..index]
          .iter()
          .any(|previous| previous.name == protocol.name)
      })
  }
}

/// Reads exactly one bounded identity frame, leaving service bytes untouched.
///
/// # Errors
/// Returns I/O or invalid-data errors for incomplete or invalid metadata.
pub async fn read_identity(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<RemoteIdentity> {
  let size = reader.read_u32().await? as usize;
  if size == 0 || size > MAX_IDENTITY_BYTES {
    return Err(invalid_identity());
  }
  let mut bytes = vec![0; size];
  reader.read_exact(&mut bytes).await?;
  let identity: RemoteIdentity = serde_json::from_slice(&bytes).map_err(|_| invalid_identity())?;
  if !identity.is_valid() {
    return Err(invalid_identity());
  }
  Ok(identity)
}

/// Writes a bounded identity frame after contract negotiation.
///
/// # Errors
/// Returns I/O or invalid-data errors for invalid metadata.
pub async fn write_identity(
  writer: &mut (impl AsyncWrite + Unpin),
  identity: &RemoteIdentity,
) -> io::Result<()> {
  if !identity.is_valid() {
    return Err(invalid_identity());
  }
  let bytes = serde_json::to_vec(identity).map_err(|_| invalid_identity())?;
  if bytes.len() > MAX_IDENTITY_BYTES {
    return Err(invalid_identity());
  }
  writer
    .write_u32(u32::try_from(bytes.len()).map_err(|_| invalid_identity())?)
    .await?;
  writer.write_all(&bytes).await
}

fn invalid_identity() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "invalid remote identity metadata",
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn older_agent_identity_has_unknown_build_metadata() {
    let identity: RemoteIdentity = serde_json::from_value(serde_json::json!({
      "remote_id": uuid::Uuid::new_v4().to_string(),
      "agent_version": "0.1.0"
    }))
    .unwrap();
    assert!(identity.is_valid());
    assert!(identity.build.is_none());
  }

  #[test]
  fn rejects_malformed_or_inconsistent_agent_build() {
    let mut identity = RemoteIdentity {
      protocols: agent_protocols(),
      remote_id: uuid::Uuid::new_v4().to_string(),
      agent_version: env!("CARGO_PKG_VERSION").into(),
      build: Some(ctl_core::component::build_info()),
      ctmux_restart_supported: false,
      bundle: None,
    };
    assert!(identity.is_valid());
    identity.build.as_mut().unwrap().source_fingerprint = "invalid".into();
    assert!(!identity.is_valid());
    identity.build = Some(ctl_core::component::build_info());
    identity.build.as_mut().unwrap().version = "0.0.0".into();
    assert!(!identity.is_valid());
  }

  #[tokio::test]
  async fn metadata_round_trip_preserves_service_bytes() {
    let identity = RemoteIdentity {
      protocols: agent_protocols(),
      remote_id: uuid::Uuid::new_v4().to_string(),
      agent_version: "0.1.0".into(),
      build: None,
      ctmux_restart_supported: false,
      bundle: None,
    };
    let mut bytes = Vec::new();
    write_identity(&mut bytes, &identity).await.unwrap();
    bytes.extend_from_slice(&[0, 255, 27]);
    let mut reader = bytes.as_slice();
    assert_eq!(read_identity(&mut reader).await.unwrap(), identity);
    assert_eq!(reader, &[0, 255, 27]);
  }

  #[tokio::test]
  async fn rejects_oversized_truncated_and_invalid_metadata() {
    for bytes in [
      u32::MAX.to_be_bytes().to_vec(),
      vec![0, 0, 0, 10, b'{'],
      vec![0, 0, 0, 2, b'{', b'}'],
    ] {
      assert!(read_identity(&mut bytes.as_slice()).await.is_err());
    }
  }
}
