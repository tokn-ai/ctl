//! Categorized failures that never expose helper diagnostics or parser data.

/// A stable credential-helper error suitable for CLI or desktop presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Error {
  pub code: &'static str,
  pub message: &'static str,
}

impl Error {
  pub(crate) const fn new(code: &'static str, message: &'static str) -> Self {
    Self { code, message }
  }
}

pub(crate) fn timeout() -> Error {
  Error::new(
    "credential_helper_timeout",
    "The saved credential request timed out. Try again.",
  )
}

pub(crate) fn unavailable() -> Error {
  Error::new(
    "credential_helper_unavailable",
    "The credential helper is unavailable. Update or rebuild ctld and try again.",
  )
}

pub(crate) fn unsupported() -> Error {
  Error::new(
    "credential_helper_unsupported",
    "The credential helper could not complete this request. Update or rebuild ctld and try again.",
  )
}

pub(crate) fn invalid_response() -> Error {
  Error::new(
    "credential_helper_invalid_response",
    "The credential helper returned an invalid response. Update or rebuild ctld and try again.",
  )
}

/// Converts a helper credential error code to a safe, categorized message.
#[must_use]
pub fn credential_error(code: &str) -> Error {
  match code {
    "credential_store_busy" => Error::new(
      "credential_store_busy",
      "Another Keychain request is still active. Complete or cancel it, then try again.",
    ),
    "credential_store_unsupported" => Error::new(
      "credentials_unsupported",
      "Saved SSH credentials require macOS Keychain.",
    ),
    "credential_request_invalid" => {
      Error::new("invalid_credential_id", "Select a valid saved credential.")
    }
    "credential_store_missing_entitlement" => Error::new(
      "credential_store_missing_entitlement",
      "This ctld helper is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile.",
    ),
    "credential_store_unavailable" => Error::new(
      "credential_store_unavailable",
      "Keychain access is unavailable. Check your macOS login session and try again.",
    ),
    "credential_store_locked" => Error::new(
      "credential_store_locked",
      "Keychain access was locked, denied, or cancelled. Unlock Keychain and allow access, then try again.",
    ),
    "credential_forget_failed" => Error::new(
      "credential_forget_failed",
      "The credential could not be removed from Keychain. Check access and try again.",
    ),
    "credential_clear_failed" => Error::new(
      "credential_clear_failed",
      "Saved SSH credentials could not be fully cleared. Some entries may already have been removed. Refresh the list and try again.",
    ),
    "credential_import_failed" => Error::new(
      "credential_import_failed",
      "Saved credential metadata could not be fully imported. Existing credentials are unchanged; try importing again.",
    ),
    "credential_not_found" => Error::new(
      "credential_not_found",
      "This saved credential no longer exists. Refresh the list.",
    ),
    _ => Error::new(
      "credentials_unavailable",
      "Could not access saved SSH credentials. Check Keychain access and try again.",
    ),
  }
}

/// Converts a helper identity error code to a safe, categorized message.
#[must_use]
pub fn identity_error(code: &str) -> Error {
  let (code, message) = match code {
    "identity_invalid_request" => (
      "identity_invalid_request",
      "Select a valid key file and enter its passphrase.",
    ),
    "identity_file_changed" => (
      "identity_file_changed",
      "The key file changed. Refresh and verify the current file before saving.",
    ),
    "identity_file_missing" => (
      "identity_file_missing",
      "The key file no longer exists. Refresh the list.",
    ),
    "identity_file_unreadable" => (
      "identity_file_unreadable",
      "The key file could not be read. Check its permissions.",
    ),
    "identity_unlock_failed" => (
      "identity_unlock_failed",
      "The passphrase could not unlock this key file.",
    ),
    "identity_keychain_missing_entitlement" => (
      "identity_keychain_missing_entitlement",
      "This ctld helper is not authorized for Keychain access. Use the signed ctld app with its matching provisioning profile.",
    ),
    "identity_keychain_unavailable" => (
      "identity_keychain_unavailable",
      "Keychain access is unavailable. Check your macOS login session and try again.",
    ),
    "identity_keychain_locked" => (
      "identity_keychain_locked",
      "Keychain access was locked, denied, or cancelled. Unlock Keychain and try again.",
    ),
    "identity_keychain_busy" => (
      "identity_keychain_busy",
      "Another Keychain request is still active. Complete or cancel it, then try again.",
    ),
    "identity_list_failed" => (
      "identity_list_failed",
      "Saved passphrase metadata could not be read. Check Keychain access and try again.",
    ),
    "identity_unsupported" => (
      "identity_unsupported",
      "This identity-file operation is not supported on this platform or for this key format.",
    ),
    "identity_forget_failed" => (
      "identity_forget_failed",
      "The saved passphrase could not be removed. Check Keychain access and try again.",
    ),
    "identity_save_failed" => (
      "identity_save_failed",
      "The verified passphrase could not be saved. Check Keychain access and try again.",
    ),
    _ => (
      "identity_list_failed",
      "Identity files could not be inspected. Update or rebuild ctld and try again.",
    ),
  };
  Error::new(code, message)
}
