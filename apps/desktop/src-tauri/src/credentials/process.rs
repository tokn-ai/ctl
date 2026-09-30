//! Bounded stdin/stdout exchange with the signed, one-shot credential helper.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::error::{CommandErrorDto, CommandResult};

static HELPER_LIMIT: Semaphore = Semaphore::const_new(2);

pub(super) struct Output {
  pub success: bool,
  pub input_result: std::io::Result<()>,
  pub bytes: Vec<u8>,
}

pub(super) async fn exchange(
  mut command: Command,
  operation: &'static str,
  request: Zeroizing<Vec<u8>>,
  maximum_output: usize,
  deadline: Duration,
) -> CommandResult<Output> {
  tokio::time::timeout(deadline, async {
    let _permit = HELPER_LIMIT.acquire().await.map_err(|_| unavailable())?;
    let mut child = command
      .arg(operation)
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_ASKPASS_TOKEN")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS_SOCKET")
      .env_remove("CTLD_IDENTITY_ASKPASS_TOKEN")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .map_err(|_| unavailable())?;
    let stdin = child.stdin.take().ok_or_else(unavailable)?;
    let stdout = child.stdout.take().ok_or_else(unavailable)?;
    let result = tokio::try_join!(
      child.wait(),
      async move {
        // A legacy helper can exit before reading. Preserve the write error
        // while collecting its status; never let it mask a structured error.
        Ok::<_, std::io::Error>(write_request(stdin, &request).await)
      },
      read_limited(stdout, maximum_output)
    );
    if let Ok((status, input_result, bytes)) = result {
      Ok(Output {
        success: status.success(),
        input_result,
        bytes,
      })
    } else {
      let _ = child.kill().await;
      Err(invalid_response())
    }
  })
  .await
  .map_err(|_| {
    CommandErrorDto::new(
      "credential_helper_timeout",
      "The saved credential request timed out. Try again.",
    )
  })?
}

async fn write_request(mut stdin: impl AsyncWrite + Unpin, bytes: &[u8]) -> std::io::Result<()> {
  stdin.write_all(bytes).await?;
  stdin.shutdown().await?;
  // EOF-reading helpers must see the pipe close before they produce a reply.
  drop(stdin);
  Ok(())
}

async fn read_limited(reader: impl AsyncRead + Unpin, maximum: usize) -> std::io::Result<Vec<u8>> {
  let mut output = Vec::new();
  reader
    .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
    .read_to_end(&mut output)
    .await?;
  if output.len() > maximum {
    return Err(std::io::Error::other("credential response exceeded limit"));
  }
  Ok(output)
}

pub(super) fn unavailable() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_unavailable",
    "The credential helper is unavailable. Update or rebuild ctld and try again.",
  )
}

pub(super) fn unsupported() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_unsupported",
    "The credential helper could not complete this request. Update or rebuild ctld and try again.",
  )
}

pub(super) fn invalid_response() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_invalid_response",
    "The credential helper returned an invalid response. Update or rebuild ctld and try again.",
  )
}
