use base64::Engine;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CONTROL_PROTOCOL_VERSION: u16 = 1;
pub const PAIRING_INVITATION_VERSION: u16 = 1;
pub const MAX_FRAME_SIZE: usize = 64 * 1024;
pub const AUTHENTICATION_DOMAIN: &[u8] = b"ctl-auth-v1\0";

/// Capability names negotiated by the outer `ctl` control protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
  RmuxTunnel,
}

/// Services that can be selected after authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
  Rmux,
}

/// A short-lived, one-time pairing invitation produced by `ctld`.
///
/// The invitation carries a pinned TLS certificate and a bearer token. It must
/// be treated as a secret until `ctl pair` consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInvitation {
  pub invitation_version: u16,
  pub endpoint: String,
  pub server_name: String,
  pub device_id: String,
  pub device_certificate_base64: String,
  pub token_base64: String,
  pub expires_at_ms: u64,
  pub client_label: String,
}

/// Encodes a pairing invitation for transfer between the target device and a
/// client. The result contains a bearer token and must be treated as secret.
///
/// # Errors
///
/// Returns an error when the invitation uses an unsupported format version or
/// cannot be serialized.
pub fn encode_pairing_invitation(
  invitation: &PairingInvitation,
) -> Result<String, InvitationError> {
  validate_pairing_invitation(invitation)?;
  let serialized = serde_json::to_vec(invitation)?;
  Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serialized))
}

/// Decodes a pairing invitation produced by [`encode_pairing_invitation`].
///
/// # Errors
///
/// Returns an error when the invitation is not URL-safe base64 JSON in the
/// expected schema.
pub fn decode_pairing_invitation(encoded: &str) -> Result<PairingInvitation, InvitationError> {
  let serialized = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded.trim())?;
  let invitation = serde_json::from_slice(&serialized)?;
  validate_pairing_invitation(&invitation)?;
  Ok(invitation)
}

/// Validates that an invitation uses a format understood by this client.
///
/// # Errors
///
/// Returns an error when the invitation format version is unsupported.
pub fn validate_pairing_invitation(invitation: &PairingInvitation) -> Result<(), InvitationError> {
  if invitation.invitation_version != PAIRING_INVITATION_VERSION {
    return Err(InvitationError::UnsupportedVersion {
      actual: invitation.invitation_version,
      supported: PAIRING_INVITATION_VERSION,
    });
  }
  Ok(())
}

#[derive(Debug, Error)]
pub enum InvitationError {
  #[error("pairing invitation version {actual} is unsupported; this client supports {supported}")]
  UnsupportedVersion { actual: u16, supported: u16 },
  #[error("invalid base64 pairing invitation: {0}")]
  Base64(#[from] base64::DecodeError),
  #[error("invalid pairing invitation JSON: {0}")]
  Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  Hello {
    protocol_version: u16,
    client_name: String,
    client_version: String,
  },
  Authenticate {
    public_key: Vec<u8>,
    signature: Vec<u8>,
  },
  Pair {
    token: Vec<u8>,
    public_key: Vec<u8>,
    label: String,
  },
  OpenService {
    service: Service,
    rmux_protocol_version: u16,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
  InvalidRequest,
  ProtocolVersionMismatch,
  AuthenticationRequired,
  AuthenticationFailed,
  PairingRejected,
  Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  HelloAccepted {
    protocol_version: u16,
    server_version: String,
    device_id: String,
    challenge: Vec<u8>,
  },
  Authenticated {
    client_id: String,
    capabilities: Vec<Capability>,
  },
  PairAccepted {
    client_id: String,
  },
  ServiceOpened {
    service: Service,
  },
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

/// Serializes and writes one length-prefixed outer-control frame.
///
/// # Errors
///
/// Returns an error when serialization fails, the encoded message exceeds the
/// outer protocol limit, or the transport cannot be written.
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

/// Reads and deserializes one length-prefixed outer-control frame.
///
/// A clean end of stream before a frame returns `Ok(None)`.
///
/// # Errors
///
/// Returns an error when the transport ends mid-frame, exceeds the frame
/// limit, or contains invalid JSON for the requested type.
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

/// Returns the canonical byte sequence an authenticated client signs.
///
/// Length-prefixing every variable-size field prevents concatenation ambiguity
/// while keeping the signed format independent of JSON serialization details.
#[must_use]
pub fn authentication_payload(
  challenge: &[u8],
  client_name: &str,
  client_version: &str,
) -> Vec<u8> {
  let mut payload = Vec::with_capacity(
    AUTHENTICATION_DOMAIN.len() + challenge.len() + client_name.len() + client_version.len() + 12,
  );
  payload.extend_from_slice(AUTHENTICATION_DOMAIN);
  append_length_prefixed(&mut payload, challenge);
  append_length_prefixed(&mut payload, client_name.as_bytes());
  append_length_prefixed(&mut payload, client_version.as_bytes());
  payload
}

/// Returns the stable, printable identity derived from an Ed25519 public key.
///
/// It is intentionally an encoding rather than an opaque mutable account ID:
/// the device authorization registry can always verify that the displayed ID
/// corresponds to the key that authenticated a connection.
#[must_use]
pub fn client_id_from_public_key(public_key: &[u8]) -> String {
  base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key)
}

fn append_length_prefixed(output: &mut Vec<u8>, value: &[u8]) {
  #[allow(clippy::cast_possible_truncation)]
  let length = value.len() as u32;
  output.extend_from_slice(&length.to_be_bytes());
  output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn control_frames_round_trip() {
    let expected = ClientMessage::OpenService {
      service: Service::Rmux,
      rmux_protocol_version: 4,
    };
    let (mut client, mut server) = tokio::io::duplex(1024);

    let write_expected = expected.clone();
    let write = tokio::spawn(async move { write_frame(&mut client, &write_expected).await });
    let actual: ClientMessage = read_frame(&mut server).await.unwrap().unwrap();

    write.await.unwrap().unwrap();
    assert_eq!(actual, expected);
  }

  #[test]
  fn authentication_payload_is_unambiguous() {
    assert_ne!(
      authentication_payload(b"ab", "c", "d"),
      authentication_payload(b"a", "bc", "d")
    );
    assert_ne!(
      authentication_payload(b"challenge", "ctl", "1"),
      authentication_payload(b"challenge", "ctl1", "")
    );
  }

  #[test]
  fn client_id_encodes_the_complete_public_key() {
    assert_eq!(client_id_from_public_key(&[0, 1, 2]), "AAEC");
    assert_ne!(
      client_id_from_public_key(&[0, 1, 2]),
      client_id_from_public_key(&[0, 1, 3])
    );
  }

  #[test]
  fn pairing_invitation_round_trips_without_exposing_its_json_shape() {
    let expected = PairingInvitation {
      invitation_version: PAIRING_INVITATION_VERSION,
      endpoint: "100.100.100.100:9944".into(),
      server_name: "device.ctl.invalid".into(),
      device_id: "device".into(),
      device_certificate_base64: "certificate".into(),
      token_base64: "token".into(),
      expires_at_ms: 42,
      client_label: "laptop".into(),
    };

    let encoded = encode_pairing_invitation(&expected).unwrap();
    assert!(!encoded.contains('{'));
    assert_eq!(decode_pairing_invitation(&encoded).unwrap(), expected);
  }

  #[test]
  fn unsupported_pairing_invitation_version_is_rejected() {
    let invitation = PairingInvitation {
      invitation_version: PAIRING_INVITATION_VERSION + 1,
      endpoint: "host:9944".into(),
      server_name: "device.ctl.invalid".into(),
      device_id: "device".into(),
      device_certificate_base64: "certificate".into(),
      token_base64: "token".into(),
      expires_at_ms: 42,
      client_label: "laptop".into(),
    };

    assert!(matches!(
      encode_pairing_invitation(&invitation),
      Err(InvitationError::UnsupportedVersion { .. })
    ));
  }
}
