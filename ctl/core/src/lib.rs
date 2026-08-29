//! Reusable authenticated client primitives for `ctl`.
//!
//! This crate owns the portable client identity, host configuration, TLS
//! connection, and outer-control handshake. It deliberately does not know how
//! configuration is stored on a particular operating system or how a terminal
//! is rendered.

use base64::{Engine, engine::general_purpose::STANDARD};
use ctl_proto::{
  CONTROL_PROTOCOL_VERSION, Capability, ClientMessage, CodecError, ErrorCode, PairingInvitation,
  ServerMessage, Service, authentication_payload, client_id_from_public_key, read_frame,
  validate_pairing_invitation, write_frame,
};
use ed25519_dalek::{Signer, SigningKey};
use rustls::{
  ClientConfig, RootCertStore,
  pki_types::{CertificateDer, ServerName},
};
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

pub const CLIENT_STATE_VERSION: u16 = 1;

/// A locally generated Ed25519 identity used for protocol authorization.
pub struct ClientIdentity {
  signing_key: SigningKey,
}

impl ClientIdentity {
  /// Generates a new independent client identity from the operating system's
  /// cryptographic random source.
  ///
  /// # Errors
  ///
  /// Returns an error when secure randomness is unavailable.
  pub fn generate() -> Result<Self, CoreError> {
    let mut private_key = [0_u8; 32];
    getrandom::fill(&mut private_key).map_err(|error| CoreError::Random(error.to_string()))?;
    Ok(Self {
      signing_key: SigningKey::from_bytes(&private_key),
    })
  }

  /// Restores an identity from exactly one Ed25519 private-key seed.
  ///
  /// # Errors
  ///
  /// Returns an error unless `private_key` contains exactly 32 bytes.
  pub fn from_private_key(private_key: &[u8]) -> Result<Self, CoreError> {
    let private_key: [u8; 32] = private_key
      .try_into()
      .map_err(|_| CoreError::InvalidPrivateKey)?;
    Ok(Self {
      signing_key: SigningKey::from_bytes(&private_key),
    })
  }

  /// Returns the private seed for owner-only local persistence.
  #[must_use]
  pub fn private_key_bytes(&self) -> [u8; 32] {
    self.signing_key.to_bytes()
  }

  /// Returns the public key supplied to `ctld` during pairing and login.
  #[must_use]
  pub fn public_key_bytes(&self) -> [u8; 32] {
    self.signing_key.verifying_key().to_bytes()
  }

  /// Returns the printable stable identity derived from this public key.
  #[must_use]
  pub fn client_id(&self) -> String {
    client_id_from_public_key(&self.public_key_bytes())
  }

  /// Signs the canonical control-authentication payload for a server challenge.
  #[must_use]
  pub fn sign_authentication(
    &self,
    challenge: &[u8],
    client_name: &str,
    client_version: &str,
  ) -> [u8; 64] {
    self
      .signing_key
      .sign(&authentication_payload(
        challenge,
        client_name,
        client_version,
      ))
      .to_bytes()
  }
}

/// A configured, certificate-pinned remote device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConfig {
  pub alias: String,
  pub endpoint: String,
  pub server_name: String,
  pub device_id: String,
  pub device_certificate_base64: String,
}

/// Portable serialized client state. The caller is responsible for persisting
/// it in platform-appropriate private storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientState {
  pub version: u16,
  pub identity_private_key_base64: String,
  pub hosts: Vec<HostConfig>,
}

impl ClientState {
  #[must_use]
  pub fn new(identity: &ClientIdentity) -> Self {
    Self {
      version: CLIENT_STATE_VERSION,
      identity_private_key_base64: STANDARD.encode(identity.private_key_bytes()),
      hosts: Vec::new(),
    }
  }

  /// Restores the client identity embedded in this state document.
  ///
  /// # Errors
  ///
  /// Returns an error when the state version or key encoding is unsupported.
  pub fn identity(&self) -> Result<ClientIdentity, CoreError> {
    if self.version != CLIENT_STATE_VERSION {
      return Err(CoreError::UnsupportedStateVersion {
        actual: self.version,
        supported: CLIENT_STATE_VERSION,
      });
    }
    let private_key = STANDARD.decode(&self.identity_private_key_base64)?;
    ClientIdentity::from_private_key(&private_key)
  }

  /// Inserts or replaces one host by its local alias.
  pub fn upsert_host(&mut self, host: HostConfig) {
    if let Some(existing) = self
      .hosts
      .iter_mut()
      .find(|existing| existing.alias == host.alias)
    {
      *existing = host;
      return;
    }
    self.hosts.push(host);
    self
      .hosts
      .sort_by(|left, right| left.alias.cmp(&right.alias));
  }

  #[must_use]
  pub fn host(&self, alias: &str) -> Option<&HostConfig> {
    self.hosts.iter().find(|host| host.alias == alias)
  }
}

/// Establishes a server-authenticated TLS connection to a certificate-pinned
/// remote device.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be reached, the pinned
/// certificate is invalid, or TLS identity verification fails.
pub async fn connect_tls(host: &HostConfig) -> Result<TlsStream<TcpStream>, CoreError> {
  let certificate = STANDARD.decode(&host.device_certificate_base64)?;
  let mut roots = RootCertStore::empty();
  roots
    .add(CertificateDer::from(certificate))
    .map_err(|error| CoreError::TlsConfiguration(error.to_string()))?;
  let config = ClientConfig::builder()
    .with_root_certificates(roots)
    .with_no_client_auth();
  let connector = TlsConnector::from(Arc::new(config));
  let server_name = ServerName::try_from(host.server_name.clone())
    .map_err(|_| CoreError::InvalidServerName(host.server_name.clone()))?;
  let stream = TcpStream::connect(&host.endpoint).await?;
  Ok(connector.connect(server_name, stream).await?)
}

/// Consumes a pairing invitation, authorizes this client identity, and returns
/// a reusable pinned host configuration.
///
/// # Errors
///
/// Returns an error when TLS, the control handshake, or pairing authorization
/// fails.
pub async fn pair(
  invitation: &PairingInvitation,
  alias: String,
  identity: &ClientIdentity,
  client_name: &str,
  client_version: &str,
) -> Result<HostConfig, CoreError> {
  validate_pairing_invitation(invitation)?;
  let host = host_from_invitation(invitation, alias);
  let mut stream = connect_tls(&host).await?;
  let hello = begin_control(&mut stream, client_name, client_version).await?;
  ensure_device_id(&host, &hello.device_id)?;

  let token = STANDARD.decode(&invitation.token_base64)?;
  write_frame(
    &mut stream,
    &ClientMessage::Pair {
      token,
      public_key: identity.public_key_bytes().to_vec(),
      label: invitation.client_label.clone(),
    },
  )
  .await?;
  match read_control_response(&mut stream).await? {
    ServerMessage::PairAccepted { client_id } if client_id == identity.client_id() => Ok(host),
    ServerMessage::PairAccepted { client_id } => Err(CoreError::UnexpectedClientId {
      expected: identity.client_id(),
      actual: client_id,
    }),
    response => Err(unexpected("pair_accepted", &response)),
  }
}

/// Opens the authenticated raw `rmux` tunnel for a configured host.
///
/// Once this function returns, the TLS stream is no longer carrying
/// `ctl-proto` frames. It carries only end-to-end `rmux-proto` frames.
///
/// # Errors
///
/// Returns an error when TLS, authentication, capability negotiation, or the
/// tunnel upgrade fails.
pub async fn open_rmux_tunnel(
  host: &HostConfig,
  identity: &ClientIdentity,
  client_name: &str,
  client_version: &str,
) -> Result<TlsStream<TcpStream>, CoreError> {
  let mut stream = connect_tls(host).await?;
  let hello = begin_control(&mut stream, client_name, client_version).await?;
  ensure_device_id(host, &hello.device_id)?;

  write_frame(
    &mut stream,
    &ClientMessage::Authenticate {
      public_key: identity.public_key_bytes().to_vec(),
      signature: identity
        .sign_authentication(&hello.challenge, client_name, client_version)
        .to_vec(),
    },
  )
  .await?;
  match read_control_response(&mut stream).await? {
    ServerMessage::Authenticated {
      client_id,
      capabilities,
    } if client_id == identity.client_id() && capabilities.contains(&Capability::RmuxTunnel) => {}
    ServerMessage::Authenticated { client_id, .. } if client_id != identity.client_id() => {
      return Err(CoreError::UnexpectedClientId {
        expected: identity.client_id(),
        actual: client_id,
      });
    }
    ServerMessage::Authenticated { .. } => return Err(CoreError::RmuxCapabilityUnavailable),
    response => return Err(unexpected("authenticated", &response)),
  }

  write_frame(
    &mut stream,
    &ClientMessage::OpenService {
      service: Service::Rmux,
      rmux_protocol_version: rmux_proto::PROTOCOL_VERSION,
    },
  )
  .await?;
  match read_control_response(&mut stream).await? {
    ServerMessage::ServiceOpened {
      service: Service::Rmux,
    } => Ok(stream),
    response => Err(unexpected("service_opened", &response)),
  }
}

#[derive(Debug)]
struct ControlHello {
  device_id: String,
  challenge: Vec<u8>,
}

async fn begin_control<S>(
  stream: &mut S,
  client_name: &str,
  client_version: &str,
) -> Result<ControlHello, CoreError>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  write_frame(
    stream,
    &ClientMessage::Hello {
      protocol_version: CONTROL_PROTOCOL_VERSION,
      client_name: client_name.into(),
      client_version: client_version.into(),
    },
  )
  .await?;
  match read_control_response(stream).await? {
    ServerMessage::HelloAccepted {
      protocol_version,
      device_id,
      challenge,
      ..
    } if protocol_version == CONTROL_PROTOCOL_VERSION => Ok(ControlHello {
      device_id,
      challenge,
    }),
    ServerMessage::HelloAccepted {
      protocol_version, ..
    } => Err(CoreError::ProtocolVersionMismatch {
      requested: CONTROL_PROTOCOL_VERSION,
      actual: protocol_version,
    }),
    response => Err(unexpected("hello_accepted", &response)),
  }
}

async fn read_control_response<S>(stream: &mut S) -> Result<ServerMessage, CoreError>
where
  S: tokio::io::AsyncRead + Unpin,
{
  match read_frame(stream).await? {
    Some(ServerMessage::Error { code, message }) => Err(CoreError::Server { code, message }),
    Some(message) => Ok(message),
    None => Err(CoreError::UnexpectedEof),
  }
}

fn host_from_invitation(invitation: &PairingInvitation, alias: String) -> HostConfig {
  HostConfig {
    alias,
    endpoint: invitation.endpoint.clone(),
    server_name: invitation.server_name.clone(),
    device_id: invitation.device_id.clone(),
    device_certificate_base64: invitation.device_certificate_base64.clone(),
  }
}

fn ensure_device_id(host: &HostConfig, actual: &str) -> Result<(), CoreError> {
  if host.device_id == actual {
    Ok(())
  } else {
    Err(CoreError::UnexpectedDeviceId {
      expected: host.device_id.clone(),
      actual: actual.into(),
    })
  }
}

fn unexpected(expected: &'static str, actual: &ServerMessage) -> CoreError {
  CoreError::UnexpectedResponse {
    expected,
    actual: format!("{actual:?}"),
  }
}

#[derive(Debug, Error)]
pub enum CoreError {
  #[error("secure randomness is unavailable: {0}")]
  Random(String),
  #[error("the saved client private key is not a 32-byte Ed25519 seed")]
  InvalidPrivateKey,
  #[error("state version {actual} is unsupported; this client supports {supported}")]
  UnsupportedStateVersion { actual: u16, supported: u16 },
  #[error(transparent)]
  Base64(#[from] base64::DecodeError),
  #[error(transparent)]
  Invitation(#[from] ctl_proto::InvitationError),
  #[error(transparent)]
  Codec(#[from] CodecError),
  #[error("network or TLS I/O error: {0}")]
  Io(#[from] io::Error),
  #[error("invalid TLS configuration: {0}")]
  TlsConfiguration(String),
  #[error("invalid TLS server name '{0}'")]
  InvalidServerName(String),
  #[error("expected device ID '{expected}', received '{actual}'")]
  UnexpectedDeviceId { expected: String, actual: String },
  #[error("expected client ID '{expected}', received '{actual}'")]
  UnexpectedClientId { expected: String, actual: String },
  #[error("control protocol mismatch: requested {requested}, server selected {actual}")]
  ProtocolVersionMismatch { requested: u16, actual: u16 },
  #[error("the device did not grant the rmux tunnel capability")]
  RmuxCapabilityUnavailable,
  #[error("device closed the control connection before responding")]
  UnexpectedEof,
  #[error("device error {code:?}: {message}")]
  Server { code: ErrorCode, message: String },
  #[error("expected {expected}, received {actual}")]
  UnexpectedResponse {
    expected: &'static str,
    actual: String,
  },
}

#[cfg(test)]
mod tests {
  use super::*;
  use ed25519_dalek::{Signature, Verifier, VerifyingKey};

  #[test]
  fn identity_round_trips_and_signs_a_challenge() {
    let identity = ClientIdentity::generate().unwrap();
    let restored = ClientIdentity::from_private_key(&identity.private_key_bytes()).unwrap();
    assert_eq!(identity.public_key_bytes(), restored.public_key_bytes());
    assert_eq!(identity.client_id(), restored.client_id());

    let challenge = b"test challenge";
    let signature = identity.sign_authentication(challenge, "ctl", "0.1.0");
    let verifying_key = VerifyingKey::from_bytes(&identity.public_key_bytes()).unwrap();
    verifying_key
      .verify(
        &authentication_payload(challenge, "ctl", "0.1.0"),
        &Signature::from_bytes(&signature),
      )
      .unwrap();
  }

  #[test]
  fn client_state_replaces_hosts_by_alias() {
    let identity = ClientIdentity::generate().unwrap();
    let mut state = ClientState::new(&identity);
    let first = HostConfig {
      alias: "mac".into(),
      endpoint: "mac.example:4433".into(),
      server_name: "device.ctl.invalid".into(),
      device_id: "device".into(),
      device_certificate_base64: "AA==".into(),
    };
    state.upsert_host(first);
    state.upsert_host(HostConfig {
      endpoint: "new-mac.example:4433".into(),
      ..state.host("mac").unwrap().clone()
    });

    assert_eq!(state.hosts.len(), 1);
    assert_eq!(state.host("mac").unwrap().endpoint, "new-mac.example:4433");
    assert_eq!(state.identity().unwrap().client_id(), identity.client_id());
  }
}
