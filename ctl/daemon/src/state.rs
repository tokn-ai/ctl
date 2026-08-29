use base64::{Engine, engine::general_purpose::STANDARD};
use ctl_proto::{PAIRING_INVITATION_VERSION, PairingInvitation};
use rcgen::{CertificateParams, KeyPair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub const DEVICE_STATE_VERSION: u16 = 1;

const DEVICE_STATE_FILE: &str = "device.json";
const SERVER_KEY_FILE: &str = "server-key.der";
const STATE_LOCK_FILE: &str = ".state.lock";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
  pub version: u16,
  pub device_id: String,
  pub server_name: String,
  pub server_certificate_base64: String,
  pub pending_pairings: Vec<PendingPairing>,
  pub authorized_clients: Vec<AuthorizedClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPairing {
  pub token_hash_base64: String,
  pub expires_at_ms: u64,
  pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorizedClient {
  pub client_id: String,
  pub public_key_base64: String,
  pub label: String,
  pub paired_at_ms: u64,
  pub revoked: bool,
}

/// Creates the persistent device identity required by `ctld`.
///
/// # Errors
///
/// Returns an error when state already exists, state storage cannot be made
/// private, or cryptographic material cannot be generated.
pub fn initialize(state_dir: &Path) -> Result<DeviceState, StateError> {
  with_state_lock(state_dir, || {
    let state_path = device_state_path(state_dir);
    if state_path.exists() {
      return Err(StateError::AlreadyInitialized(state_dir.to_path_buf()));
    }

    let device_id = Uuid::new_v4().to_string();
    let server_name = format!("{device_id}.ctl.invalid");
    let parameters = CertificateParams::new(vec![server_name.clone()])?;
    let private_key = KeyPair::generate()?;
    let certificate = parameters.self_signed(&private_key)?;
    let state = DeviceState {
      version: DEVICE_STATE_VERSION,
      device_id,
      server_name,
      server_certificate_base64: STANDARD.encode(certificate.der().as_ref()),
      pending_pairings: Vec::new(),
      authorized_clients: Vec::new(),
    };

    write_private_file(&server_key_path(state_dir), &private_key.serialize_der())?;
    if let Err(error) = write_state(state_dir, &state) {
      let _ = fs::remove_file(server_key_path(state_dir));
      return Err(error);
    }
    Ok(state)
  })
}

/// Loads and validates the device state from owner-only storage.
///
/// # Errors
///
/// Returns an error when the state is absent, unsafe, malformed, or for an
/// unsupported version.
pub fn load(state_dir: &Path) -> Result<DeviceState, StateError> {
  prepare_state_directory(state_dir)?;
  let state_path = device_state_path(state_dir);
  ensure_private_file(&state_path)?;
  let state = serde_json::from_slice(&fs::read(state_path)?)?;
  validate_state(&state)?;
  Ok(state)
}

/// Returns the TLS server private key after validating owner-only storage.
///
/// # Errors
///
/// Returns an error when the key is absent, unsafe, or unreadable.
pub fn load_server_key(state_dir: &Path) -> Result<Vec<u8>, StateError> {
  let key_path = server_key_path(state_dir);
  ensure_private_file(&key_path)?;
  Ok(fs::read(key_path)?)
}

/// Creates and records a one-time invitation, retaining only its SHA-256
/// digest in device state.
///
/// # Errors
///
/// Returns an error when secure randomness is unavailable, the label is
/// invalid, or state persistence fails.
pub fn create_pairing_invitation(
  state_dir: &Path,
  endpoint: String,
  label: String,
  expires_at_ms: u64,
) -> Result<PairingInvitation, StateError> {
  validate_label(&label)?;
  if endpoint.trim().is_empty() {
    return Err(StateError::InvalidEndpoint);
  }
  if expires_at_ms <= now_ms()? {
    return Err(StateError::ExpirationInPast);
  }

  with_state_lock(state_dir, || {
    let mut state = load(state_dir)?;
    let now = now_ms()?;
    state
      .pending_pairings
      .retain(|pairing| pairing.expires_at_ms > now);

    let mut token = [0_u8; 32];
    getrandom::fill(&mut token).map_err(|error| StateError::Random(error.to_string()))?;
    state.pending_pairings.push(PendingPairing {
      token_hash_base64: STANDARD.encode(Sha256::digest(token)),
      expires_at_ms,
      label: label.clone(),
    });
    validate_state(&state)?;
    write_state(state_dir, &state)?;

    Ok(PairingInvitation {
      invitation_version: PAIRING_INVITATION_VERSION,
      endpoint,
      server_name: state.server_name,
      device_id: state.device_id,
      device_certificate_base64: state.server_certificate_base64,
      token_base64: STANDARD.encode(token),
      expires_at_ms,
      client_label: label,
    })
  })
}

/// Consumes a valid pairing invitation and authorizes an Ed25519 public key.
///
/// The pairing token is removed before the state is saved, making it
/// single-use even when the supplied key was already authorized.
///
/// # Errors
///
/// Returns an error when the token is missing, expired, does not match the
/// invitation label, or persistence fails.
pub fn consume_pairing(
  state_dir: &Path,
  token: &[u8],
  public_key: &[u8],
  label: &str,
) -> Result<AuthorizedClient, StateError> {
  validate_label(label)?;
  if public_key.len() != 32 {
    return Err(StateError::InvalidPublicKey);
  }

  with_state_lock(state_dir, || {
    let now = now_ms()?;
    let token_hash = STANDARD.encode(Sha256::digest(token));
    let mut state = load(state_dir)?;
    state
      .pending_pairings
      .retain(|pairing| pairing.expires_at_ms > now);
    let index = state
      .pending_pairings
      .iter()
      .position(|pairing| pairing.token_hash_base64 == token_hash)
      .ok_or(StateError::PairingRejected)?;
    let pairing = state.pending_pairings.remove(index);
    if pairing.label != label {
      validate_state(&state)?;
      write_state(state_dir, &state)?;
      return Err(StateError::PairingRejected);
    }

    let public_key_base64 = STANDARD.encode(public_key);
    let client_id = ctl_proto::client_id_from_public_key(public_key);
    let client = AuthorizedClient {
      client_id: client_id.clone(),
      public_key_base64,
      label: pairing.label,
      paired_at_ms: now,
      revoked: false,
    };
    if let Some(existing) = state
      .authorized_clients
      .iter_mut()
      .find(|existing| existing.client_id == client_id)
    {
      *existing = client.clone();
    } else {
      state.authorized_clients.push(client.clone());
      state
        .authorized_clients
        .sort_by(|left, right| left.client_id.cmp(&right.client_id));
    }
    validate_state(&state)?;
    write_state(state_dir, &state)?;
    Ok(client)
  })
}

#[must_use]
pub fn device_state_path(state_dir: &Path) -> PathBuf {
  state_dir.join(DEVICE_STATE_FILE)
}

fn server_key_path(state_dir: &Path) -> PathBuf {
  state_dir.join(SERVER_KEY_FILE)
}

fn now_ms() -> Result<u64, StateError> {
  let duration = SystemTime::now().duration_since(UNIX_EPOCH)?;
  u64::try_from(duration.as_millis()).map_err(|_| StateError::ClockOverflow)
}

fn validate_state(state: &DeviceState) -> Result<(), StateError> {
  if state.version != DEVICE_STATE_VERSION {
    return Err(StateError::UnsupportedVersion {
      actual: state.version,
      supported: DEVICE_STATE_VERSION,
    });
  }
  if state.device_id.is_empty() || state.server_name.is_empty() {
    return Err(StateError::MalformedState("device identity is empty"));
  }
  STANDARD
    .decode(&state.server_certificate_base64)
    .map_err(StateError::Base64)?;
  for pairing in &state.pending_pairings {
    validate_label(&pairing.label)?;
    let digest = STANDARD
      .decode(&pairing.token_hash_base64)
      .map_err(StateError::Base64)?;
    if digest.len() != 32 {
      return Err(StateError::MalformedState(
        "pairing token digest is not 32 bytes",
      ));
    }
  }
  for client in &state.authorized_clients {
    validate_label(&client.label)?;
    let key = STANDARD
      .decode(&client.public_key_base64)
      .map_err(StateError::Base64)?;
    if key.len() != 32 || ctl_proto::client_id_from_public_key(&key) != client.client_id {
      return Err(StateError::MalformedState(
        "authorized client key is invalid",
      ));
    }
  }
  Ok(())
}

fn validate_label(label: &str) -> Result<(), StateError> {
  let trimmed = label.trim();
  if trimmed.is_empty() || trimmed.len() > 64 || trimmed.chars().any(char::is_control) {
    return Err(StateError::InvalidLabel);
  }
  Ok(())
}

fn prepare_state_directory(state_dir: &Path) -> Result<(), StateError> {
  let existed = state_dir.exists();
  fs::create_dir_all(state_dir)?;
  if !existed {
    set_owner_only_directory(state_dir)?;
  }
  ensure_private_directory(state_dir)?;
  Ok(())
}

fn with_state_lock<T>(
  state_dir: &Path,
  operation: impl FnOnce() -> Result<T, StateError>,
) -> Result<T, StateError> {
  prepare_state_directory(state_dir)?;
  let lock_path = state_dir.join(STATE_LOCK_FILE);
  let lock_file = open_private_lock_file(&lock_path)?;
  lock_state_file(&lock_file)?;
  operation()
}

fn open_private_lock_file(path: &Path) -> Result<File, StateError> {
  if path.exists() {
    ensure_private_file(path)?;
  }
  let mut options = OpenOptions::new();
  options.read(true).write(true).create(true);
  set_private_file_mode(&mut options);
  let file = options.open(path)?;
  ensure_private_file(path)?;
  Ok(file)
}

#[cfg(unix)]
fn lock_state_file(file: &File) -> Result<(), StateError> {
  rustix::fs::flock(file, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)?;
  Ok(())
}

#[cfg(not(unix))]
fn lock_state_file(_file: &File) -> Result<(), StateError> {
  Ok(())
}

fn write_state(state_dir: &Path, state: &DeviceState) -> Result<(), StateError> {
  let payload = serde_json::to_vec_pretty(state)?;
  let path = device_state_path(state_dir);
  if path.exists() {
    ensure_private_file(&path)?;
  }
  let temporary = state_dir.join(format!(".device-{}.tmp", Uuid::new_v4()));
  write_private_file(&temporary, &payload)?;
  fs::rename(temporary, path)?;
  Ok(())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), StateError> {
  let mut options = OpenOptions::new();
  options.write(true).create_new(true);
  set_private_file_mode(&mut options);
  let mut file = options.open(path)?;
  file.write_all(contents)?;
  file.sync_all()?;
  ensure_private_file(path)?;
  Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(options: &mut OpenOptions) {
  use std::os::unix::fs::OpenOptionsExt;

  options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_file_mode(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), StateError> {
  use std::os::unix::fs::PermissionsExt;

  fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> Result<(), StateError> {
  Ok(())
}

#[cfg(unix)]
fn ensure_private_directory(path: &Path) -> Result<(), StateError> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};

  let metadata = fs::symlink_metadata(path)?;
  if metadata.file_type().is_symlink() || !metadata.is_dir() {
    return Err(StateError::UnsafeDirectory(path.to_path_buf()));
  }
  if metadata.uid() != rustix::process::getuid().as_raw() {
    return Err(StateError::UnsafeDirectory(path.to_path_buf()));
  }
  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(StateError::UnsafeDirectory(path.to_path_buf()));
  }
  Ok(())
}

#[cfg(not(unix))]
fn ensure_private_directory(path: &Path) -> Result<(), StateError> {
  if fs::symlink_metadata(path)?.file_type().is_symlink() {
    return Err(StateError::UnsafeDirectory(path.to_path_buf()));
  }
  Ok(())
}

#[cfg(unix)]
fn ensure_private_file(path: &Path) -> Result<(), StateError> {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};

  let metadata = fs::symlink_metadata(path)?;
  if metadata.file_type().is_symlink() || !metadata.is_file() {
    return Err(StateError::UnsafeFile(path.to_path_buf()));
  }
  if metadata.uid() != rustix::process::getuid().as_raw() {
    return Err(StateError::UnsafeFile(path.to_path_buf()));
  }
  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(StateError::UnsafeFile(path.to_path_buf()));
  }
  Ok(())
}

#[cfg(not(unix))]
fn ensure_private_file(path: &Path) -> Result<(), StateError> {
  let metadata = fs::symlink_metadata(path)?;
  if metadata.file_type().is_symlink() || !metadata.is_file() {
    return Err(StateError::UnsafeFile(path.to_path_buf()));
  }
  Ok(())
}

#[derive(Debug, Error)]
pub enum StateError {
  #[error("ctld is already initialized in {0}")]
  AlreadyInitialized(PathBuf),
  #[error("state directory {0} is not a private, owner-only directory")]
  UnsafeDirectory(PathBuf),
  #[error("state file {0} is not a private, owner-only regular file")]
  UnsafeFile(PathBuf),
  #[error("state version {actual} is unsupported; this daemon supports {supported}")]
  UnsupportedVersion { actual: u16, supported: u16 },
  #[error("state is malformed: {0}")]
  MalformedState(&'static str),
  #[error(
    "pairing labels must be non-empty, at most 64 characters, and contain no control characters"
  )]
  InvalidLabel,
  #[error("pairing endpoint cannot be empty")]
  InvalidEndpoint,
  #[error("pairing expiration must be in the future")]
  ExpirationInPast,
  #[error("pairing token was rejected")]
  PairingRejected,
  #[error("client public key is not an Ed25519 32-byte key")]
  InvalidPublicKey,
  #[error("system time is before the Unix epoch")]
  ClockBeforeEpoch(#[from] std::time::SystemTimeError),
  #[error("system time does not fit in milliseconds")]
  ClockOverflow,
  #[error("secure randomness is unavailable: {0}")]
  Random(String),
  #[error(transparent)]
  Base64(base64::DecodeError),
  #[error(transparent)]
  Io(#[from] io::Error),
  #[error(transparent)]
  Json(#[from] serde_json::Error),
  #[error(transparent)]
  Rcgen(#[from] rcgen::Error),
}

#[cfg(test)]
mod tests {
  use super::*;

  fn temporary_state_directory() -> PathBuf {
    std::env::temp_dir().join(format!("ctld-state-test-{}", Uuid::new_v4()))
  }

  #[test]
  fn pairing_tokens_are_hashed_single_use_and_label_bound() {
    let directory = temporary_state_directory();
    initialize(&directory).unwrap();
    let invitation = create_pairing_invitation(
      &directory,
      "127.0.0.1:9944".into(),
      "laptop".into(),
      now_ms().unwrap() + 60_000,
    )
    .unwrap();
    let token = STANDARD.decode(invitation.token_base64).unwrap();
    let state = load(&directory).unwrap();
    assert_ne!(
      state.pending_pairings[0].token_hash_base64,
      STANDARD.encode(&token)
    );

    assert!(matches!(
      consume_pairing(&directory, &token, &[7; 32], "wrong"),
      Err(StateError::PairingRejected)
    ));
    assert!(matches!(
      consume_pairing(&directory, &token, &[7; 32], "laptop"),
      Err(StateError::PairingRejected)
    ));

    let _ = fs::remove_dir_all(directory);
  }

  #[test]
  fn state_directory_must_be_private_on_unix() {
    let directory = temporary_state_directory();
    initialize(&directory).unwrap();
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;

      fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
      assert!(matches!(
        load(&directory),
        Err(StateError::UnsafeDirectory(_))
      ));
    }
    let _ = fs::remove_dir_all(directory);
  }
}
