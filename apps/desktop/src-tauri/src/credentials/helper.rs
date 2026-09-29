//! The signed helper owns Keychain access; the app receives metadata only.

use std::process::Stdio;
use std::time::Duration;

use ctld_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Request, Response};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::error::{CommandErrorDto, CommandResult};

#[cfg(target_os = "macos")]
// Leave time for a user to answer a Keychain unlock or access prompt.
const HELPER_TIMEOUT: Duration = Duration::from_secs(60);
static HELPER_LIMIT: Semaphore = Semaphore::const_new(2);

#[cfg(target_os = "macos")]
pub(super) async fn request(request: Request) -> CommandResult<Response> {
  let executable = ctld_ipc::daemon_executable().map_err(|_| unavailable())?;
  exchange(Command::new(executable), request, HELPER_TIMEOUT).await
}

pub(super) async fn exchange(
  mut command: Command,
  request: Request,
  deadline: Duration,
) -> CommandResult<Response> {
  let bytes = serde_json::to_vec(&request).map_err(|_| invalid_response())?;
  if bytes.len() > MAX_REQUEST_BYTES {
    return Err(CommandErrorDto::new(
      "invalid_credential_id",
      "Select a valid saved credential.",
    ));
  }
  tokio::time::timeout(deadline, async {
    let _permit = HELPER_LIMIT.acquire().await.map_err(|_| unavailable())?;
    let mut child = command
      .arg("--credential-request")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_ASKPASS_TOKEN")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .map_err(|_| unavailable())?;
    let mut stdin = child.stdin.take().ok_or_else(unavailable)?;
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let exchange = async {
      let (status, (), output) = tokio::try_join!(
        child.wait(),
        async move {
          stdin.write_all(&bytes).await?;
          stdin.shutdown().await?;
          drop(stdin);
          Ok(())
        },
        read_limited(stdout)
      )?;
      Ok::<_, std::io::Error>((status, output))
    }
    .await;
    let Ok((status, output)) = exchange else {
      let _ = child.kill().await;
      return Err(invalid_response());
    };
    let response: Response = serde_json::from_slice(&output).map_err(|_| {
      if status.success() {
        invalid_response()
      } else {
        unsupported_helper()
      }
    })?;
    if let Response::Error { code, .. } = response {
      return Err(sanitized_error(&code));
    }
    if !status.success() {
      return Err(invalid_response());
    }
    Ok(response)
  })
  .await
  .map_err(|_| {
    CommandErrorDto::new(
      "credential_helper_timeout",
      "The saved credential request timed out. Try again.",
    )
  })?
}

async fn read_limited(reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
  let mut output = Vec::new();
  reader
    .take(u64::try_from(MAX_RESPONSE_BYTES).unwrap_or(u64::MAX) + 1)
    .read_to_end(&mut output)
    .await?;
  if output.len() > MAX_RESPONSE_BYTES {
    return Err(std::io::Error::other("credential response exceeded limit"));
  }
  Ok(output)
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

fn unavailable() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_unavailable",
    "The credential helper is unavailable. Update or rebuild ctld and try again.",
  )
}

fn unsupported_helper() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_unsupported",
    "The credential helper could not complete this request. Update or rebuild ctld and try again.",
  )
}

fn invalid_response() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_invalid_response",
    "The credential helper returned an invalid response. Update or rebuild ctld and try again.",
  )
}
