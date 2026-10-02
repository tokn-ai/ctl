//! Identity snapshots and local verification. Inventory never retrieves secrets.

#[cfg(unix)]
mod agent;
mod files;
mod inventory;
mod public_hint;
mod request;

#[cfg(unix)]
pub use agent::{LocalAgent, askpass_exit_code, run_lifetime};
pub use files::{IdentitySnapshot, inspect_path};
#[cfg(target_os = "macos")]
pub(crate) use public_hint::{SavedPublicKeyHint, saved_public_key_hint_checked};
pub use public_hint::{public_key_hint, saved_public_key_hint};
pub use request::run;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
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
  #[error(
    "This ctld process is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile."
  )]
  KeychainMissingEntitlement,
  #[error("Keychain access is unavailable. Check your macOS login session and try again.")]
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
      Self::KeychainMissingEntitlement => "identity_keychain_missing_entitlement",
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
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub public_key: Option<String>,
}

impl SavedIdentity {
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
      && self.public_key.as_deref().is_none_or(|key| {
        public_hint::parse(key).is_some_and(|public| {
          public.canonical == key
            && public.key_type == self.key_type
            && public.fingerprint == self.fingerprint
        })
      })
  }

  pub(crate) fn matches(&self, snapshot: &IdentitySnapshot) -> bool {
    self.valid(&snapshot.identity_id)
      && self.path == snapshot.path
      && self.file_version == snapshot.file_version
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
  saved_passphrase_inner(snapshot, context, None)
}

/// Read for a live unlock attempt, skipping authentication if it is canceled
/// before acquiring the cross-process Keychain operation lock. This does not
/// dismiss a system authentication prompt that has already started.
///
/// # Errors
/// Returns a sanitized Keychain, changed-file, or canceled-unlock error.
pub fn saved_passphrase_cancellable(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
  canceled: &AtomicBool,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  saved_passphrase_inner(snapshot, context, Some(canceled))
}

fn saved_passphrase_inner(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
  canceled: Option<&AtomicBool>,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  if canceled.is_some_and(|canceled| canceled.load(Ordering::Acquire)) {
    return Err(IdentityError::UnlockFailed);
  }
  ensure_current(snapshot)?;
  #[cfg(target_os = "macos")]
  return crate::keychain::identity::load(snapshot, context, canceled);
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
