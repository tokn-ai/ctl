//! Bounded, private-pipe requests to the selected local credential helper.
//!
//! Helper discovery preserves the client's registered selection policy. These
//! requests never start a daemon, return stored secrets, or forward diagnostics
//! from the helper to the caller.

use std::time::Duration;

use ctl_ipc::{credentials, identities};
use tokio::process::Command;
use zeroize::Zeroizing;

mod errors;
mod process;
#[cfg(all(test, unix))]
mod tests;

pub use errors::{Error, credential_error, identity_error};
pub use process::{Output, drain_exchanges, exchange};

const PREPARATION_TIMEOUT: Duration = Duration::from_secs(30);
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const INTERACTIVE_TIMEOUT: Duration = Duration::from_secs(60);
const PREPARATION_TIMEOUT_MESSAGE: &str = "Verifying the ctld helper timed out. Wait for any ctl setup or signing operation to finish, then try again.";

/// Sends a credential operation to the selected local helper on macOS.
///
/// `ListMetadata` stays noninteractive; it never falls back to legacy `List`.
///
/// # Errors
/// Returns categorized platform, discovery, transport, or Keychain errors.
pub async fn request_credentials(
  request: credentials::Request,
) -> Result<credentials::Response, Error> {
  if !cfg!(target_os = "macos") {
    return Err(credential_error("credential_store_unsupported"));
  }
  let executable = executable().await?;
  let deadline = if matches!(request, credentials::Request::ListMetadata) {
    METADATA_TIMEOUT
  } else {
    INTERACTIVE_TIMEOUT
  };
  exchange_credentials(Command::new(executable), request, deadline).await
}

/// Sends an identity-file operation to the selected local helper.
///
/// Metadata inspection stays noninteractive; it never falls back to legacy
/// `List`, which can authenticate with Keychain in older helper versions.
///
/// # Errors
/// Returns categorized platform, discovery, transport, or identity errors.
pub async fn request_identity(request: identities::Request) -> Result<identities::Response, Error> {
  if !cfg!(unix) {
    return Err(identity_error("identity_unsupported"));
  }
  let executable = executable().await?;
  let deadline = if matches!(request, identities::Request::ListMetadata { .. }) {
    METADATA_TIMEOUT
  } else {
    INTERACTIVE_TIMEOUT
  };
  exchange_identity(Command::new(executable), request, deadline).await
}

async fn executable() -> Result<std::path::PathBuf, Error> {
  tokio::time::timeout(PREPARATION_TIMEOUT, ctl_ipc::prepare_daemon_executable())
    .await
    .map_err(|_| Error::new("credential_helper_timeout", PREPARATION_TIMEOUT_MESSAGE))?
    .map_err(|error| {
      if matches!(error, ctl_ipc::ConnectError::PrepareDaemon(source) if source.kind() == std::io::ErrorKind::TimedOut) {
        Error::new("credential_helper_timeout", PREPARATION_TIMEOUT_MESSAGE)
      } else {
        errors::unavailable()
      }
    })
}

async fn exchange_credentials(
  command: Command,
  request: credentials::Request,
  deadline: Duration,
) -> Result<credentials::Response, Error> {
  let requires_supported_operation = matches!(
    request,
    credentials::Request::ListMetadata
      | credentials::Request::ImportMetadata
      | credentials::Request::Clear {}
      | credentials::Request::Discover {}
  );
  let bytes = Zeroizing::new(serde_json::to_vec(&request).map_err(|_| errors::invalid_response())?);
  if bytes.len() > credentials::MAX_REQUEST_BYTES {
    return Err(credential_error("credential_request_invalid"));
  }
  let output = exchange(
    command,
    "--credential-request",
    bytes,
    credentials::MAX_RESPONSE_BYTES,
    deadline,
  )
  .await?;
  let response: credentials::Response = parse_response(&output)?;
  if let credentials::Response::Error { code, .. } = response {
    if requires_supported_operation && code == "credential_request_invalid" {
      return Err(errors::unsupported());
    }
    return Err(credential_error(&code));
  }
  validate_completion(&output)?;
  Ok(response)
}

async fn exchange_identity(
  command: Command,
  request: identities::Request,
  deadline: Duration,
) -> Result<identities::Response, Error> {
  let requires_metadata_support = matches!(request, identities::Request::ListMetadata { .. });
  let bytes = Zeroizing::new(
    serde_json::to_vec(&request).map_err(|_| identity_error("identity_invalid_request"))?,
  );
  drop(request);
  if bytes.len() > identities::MAX_REQUEST_BYTES {
    return Err(identity_error("identity_invalid_request"));
  }
  let output = exchange(
    command,
    "--identity-request",
    bytes,
    identities::MAX_RESPONSE_BYTES,
    deadline,
  )
  .await?;
  let response: identities::Response = parse_response(&output)?;
  if let identities::Response::Error { code, .. } = response {
    if requires_metadata_support && code == "identity_invalid_request" {
      return Err(errors::unsupported());
    }
    return Err(identity_error(&code));
  }
  validate_completion(&output)?;
  Ok(response)
}

fn parse_response<T: serde::de::DeserializeOwned>(output: &Output) -> Result<T, Error> {
  serde_json::from_slice(&output.bytes).map_err(|_| {
    if output.success {
      errors::invalid_response()
    } else {
      errors::unsupported()
    }
  })
}

fn validate_completion(output: &Output) -> Result<(), Error> {
  if !output.success || output.input_result.is_err() {
    return Err(errors::invalid_response());
  }
  Ok(())
}
