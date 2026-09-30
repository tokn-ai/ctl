//! Identity snapshots and local verification. Inventory never retrieves secrets.

#[cfg(unix)]
mod agent;
mod files;
mod inventory;
mod request;

#[cfg(unix)]
pub use agent::{LocalAgent, askpass_exit_code, run_lifetime};
pub use files::{IdentitySnapshot, inspect_path};
pub use request::run;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct VerifiedIdentity {
  pub key_type: String,
  pub fingerprint: String,
  pub public_key: String,
  identity_id: String,
  file_version: String,
  passphrase_digest: Zeroizing<[u8; 32]>,
}

fn passphrase_digest(passphrase: &str) -> Zeroizing<[u8; 32]> {
  Zeroizing::new(Sha256::digest(passphrase.as_bytes()).into())
}

#[cfg(not(unix))]
#[must_use]
pub fn askpass_exit_code() -> Option<i32> {
  None
}

/// The local-agent lifetime helper is available on Unix only.
///
/// # Errors
/// Always reports unsupported on other platforms.
#[cfg(not(unix))]
pub fn run_lifetime() -> std::io::Result<()> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "local identity agents require Unix",
  ))
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum IdentityError {
  #[error("The identity request is invalid.")]
  InvalidRequest,
  #[error("The identity file no longer exists.")]
  MissingFile,
  #[error("The identity file could not be read.")]
  UnreadableFile,
  #[error("This file is not a supported private SSH identity.")]
  UnsupportedFile,
  #[error("The identity file changed. Refresh before saving its passphrase.")]
  FileChanged,
  #[error("The passphrase could not unlock this identity file locally.")]
  UnlockFailed,
  #[error("Keychain access is unavailable. Use a signed ctld with its Keychain entitlement.")]
  KeychainUnavailable,
  #[error("Keychain access is locked or was not allowed.")]
  KeychainLocked,
  #[error("Another Keychain request is still active. Complete or cancel it, then try again.")]
  KeychainBusy,
  #[error("The identity passphrase could not be saved.")]
  SaveFailed,
  #[error("The saved identity passphrase could not be forgotten.")]
  ForgetFailed,
  #[error("Saved identity metadata could not be read.")]
  ListFailed,
}

impl IdentityError {
  #[allow(clippy::needless_pass_by_value)] // Adapter passed directly to Result::map_err.
  pub(super) fn file_io(error: std::io::Error) -> Self {
    if error.kind() == std::io::ErrorKind::NotFound {
      Self::MissingFile
    } else {
      Self::UnreadableFile
    }
  }

  #[must_use]
  pub fn code(self) -> &'static str {
    match self {
      Self::InvalidRequest => "identity_invalid_request",
      Self::MissingFile => "identity_file_missing",
      Self::UnreadableFile => "identity_file_unreadable",
      Self::UnsupportedFile => "identity_unsupported",
      Self::FileChanged => "identity_file_changed",
      Self::UnlockFailed => "identity_unlock_failed",
      Self::KeychainUnavailable => "identity_keychain_unavailable",
      Self::KeychainLocked => "identity_keychain_locked",
      Self::KeychainBusy => "identity_keychain_busy",
      Self::SaveFailed => "identity_save_failed",
      Self::ForgetFailed => "identity_forget_failed",
      Self::ListFailed => "identity_list_failed",
    }
  }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedIdentity {
  pub version: u8,
  pub path: String,
  pub file_version: String,
  pub key_type: String,
  pub fingerprint: String,
}

impl SavedIdentity {
  #[cfg(any(target_os = "macos", test))]
  pub(crate) fn valid(&self, identity_id: &str) -> bool {
    self.version == 1
      && files::valid_id(identity_id)
      && files::valid_id(&self.file_version)
      && files::expand_path(&self.path).is_ok()
      && files::digest(self.path.as_bytes()) == identity_id
      && self.key_type.len() <= 128
      && self.fingerprint.len() <= 128
      && !self.key_type.chars().any(char::is_control)
      && !self.fingerprint.chars().any(char::is_control)
  }
}

/// Read a saved passphrase only when the exact key file is still present.
/// `context` names the locally configured connection in the authentication
/// request. It must not come from an SSH server's prompt or diagnostic output.
///
/// # Errors
/// Returns a sanitized Keychain error or a changed-file error.
pub fn saved_passphrase(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  ensure_current(snapshot)?;
  #[cfg(target_os = "macos")]
  return crate::keychain::identity::load(snapshot, context);
  #[cfg(not(target_os = "macos"))]
  {
    let _ = context;
    Ok(None)
  }
}

/// Persist a passphrase after local verification, bound to the exact file bytes.
///
/// # Errors
/// Returns a sanitized error if the file changed or Keychain cannot save it.
pub fn save_verified(
  snapshot: &IdentitySnapshot,
  verified: &VerifiedIdentity,
  passphrase: &str,
) -> Result<(), IdentityError> {
  ensure_current(snapshot)?;
  if !snapshot.encrypted
    || snapshot.identity_id != verified.identity_id
    || snapshot.file_version != verified.file_version
    || passphrase_digest(passphrase) != verified.passphrase_digest
    || snapshot
      .fingerprint
      .as_ref()
      .is_some_and(|expected| expected != &verified.fingerprint)
  {
    return Err(IdentityError::InvalidRequest);
  }
  #[cfg(target_os = "macos")]
  return crate::keychain::identity::save(snapshot, verified, passphrase);
  #[cfg(not(target_os = "macos"))]
  {
    let _ = passphrase;
    Err(IdentityError::KeychainUnavailable)
  }
}

pub(super) fn ensure_current(snapshot: &IdentitySnapshot) -> Result<(), IdentityError> {
  let current = inspect_path(&snapshot.path).map_err(|_| IdentityError::FileChanged)?;
  if current.identity_id != snapshot.identity_id || current.file_version != snapshot.file_version {
    return Err(IdentityError::FileChanged);
  }
  Ok(())
}
