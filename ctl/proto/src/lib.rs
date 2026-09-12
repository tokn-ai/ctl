//! Versioned metadata preceding the service protocol on identified SSH streams.
use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const IDENTITY_PREFACE: &[u8] = b"ctl-ssh-v2\n";
const MAX_IDENTITY_BYTES: usize = 8192;

/// Stable identity of a remote user's ctl environment, independent of its address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteIdentity {
  pub remote_id: String,
  pub agent_version: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub bundle: Option<Box<BundleVersion>>,
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

/// Writes a bounded identity frame after the v2 preface.
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

  #[tokio::test]
  async fn metadata_round_trip_preserves_service_bytes() {
    let identity = RemoteIdentity {
      remote_id: uuid::Uuid::new_v4().to_string(),
      agent_version: "0.1.0".into(),
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
