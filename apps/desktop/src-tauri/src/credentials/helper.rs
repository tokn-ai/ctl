//! The signed helper owns Keychain access; the app receives metadata only.

use std::time::Duration;

use ctld_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response};
use tokio::process::Command;
use zeroize::Zeroizing;

use super::process::{self, invalid_response, unavailable, unsupported as unsupported_helper};

use crate::error::{CommandErrorDto, CommandResult};

#[cfg(target_os = "macos")]
// Leave time for a user to answer a Keychain unlock or access prompt.
const HELPER_TIMEOUT: Duration = Duration::from_mins(1);

#[cfg(target_os = "macos")]
pub(super) async fn request(request: Request) -> CommandResult<Response> {
  let executable = ctld_ipc::daemon_executable().map_err(|_| unavailable())?;
  exchange(Command::new(executable), request, HELPER_TIMEOUT).await
}

pub(super) async fn exchange(
  command: Command,
  request: Request,
  deadline: Duration,
) -> CommandResult<Response> {
  let bytes = Zeroizing::new(serde_json::to_vec(&request).map_err(|_| invalid_response())?);
  if bytes.len() > MAX_REQUEST_BYTES {
    return Err(CommandErrorDto::new(
      "invalid_credential_id",
      "Select a valid saved credential.",
    ));
  }
  let output = process::exchange(
    command,
    "--credential-request",
    bytes,
    MAX_RESPONSE_BYTES,
    deadline,
  )
  .await?;
  response_from_output(output.success, output.input_result, &output.bytes)
}

pub(super) fn response_from_output(
  status_success: bool,
  input_result: std::io::Result<()>,
  output: &[u8],
) -> CommandResult<Response> {
  let response: Response = serde_json::from_slice(output).map_err(|_| {
    if status_success {
      invalid_response()
    } else {
      unsupported_helper()
    }
  })?;
  if let Response::Error { code, .. } = response {
    return Err(sanitized_error(&code));
  }
  if !status_success {
    return Err(invalid_response());
  }
  // A response cannot confirm successful handling of an incomplete request.
  input_result.map_err(|_| invalid_response())?;
  Ok(response)
}

pub(super) fn sanitized_error(code: &str) -> CommandErrorDto {
  match code {
    "credential_store_unsupported" => CommandErrorDto::new(
      "credentials_unsupported",
      "Saved SSH credentials require macOS Keychain.",
    ),
    "credential_request_invalid" => {
      CommandErrorDto::new("invalid_credential_id", "Select a valid saved credential.")
    }
    "credential_store_unavailable" => CommandErrorDto::new(
      "credential_store_unavailable",
      "This ctld helper cannot access the credential Keychain group. Use a properly signed app and ctld helper.",
    ),
    "credential_store_locked" => CommandErrorDto::new(
      "credential_store_locked",
      "Keychain access was locked, denied, or cancelled. Unlock Keychain and allow access, then try again.",
    ),
    "credential_forget_failed" => CommandErrorDto::new(
      "credential_forget_failed",
      "The credential could not be removed from Keychain. Check access and try again.",
    ),
    "credential_not_found" => CommandErrorDto::new(
      "credential_not_found",
      "This saved credential no longer exists. Refresh the list.",
    ),
    _ => CommandErrorDto::new(
      "credentials_unavailable",
      "Could not access saved SSH credentials. Check Keychain access and try again.",
    ),
  }
}
