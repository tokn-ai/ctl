use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use tokio::sync::{Semaphore, oneshot};
use zeroize::Zeroizing;

use super::errors::{self, Error};

const HELPER_CAPACITY: u32 = 2;
static HELPER_LIMIT: Semaphore = Semaphore::const_new(HELPER_CAPACITY as usize);

/// Waits for outstanding helper exchanges and cancellation cleanup to finish.
///
/// Before shutting down a CLI runtime, drop its request futures and await this
/// drain so their supervisors can terminate and reap the owned helper children.
/// Active exchanges are also awaited; this function does not cancel them.
///
/// # Errors
/// Returns a sanitized availability error if the shared process limit closes.
pub async fn drain_exchanges() -> Result<(), Error> {
  let _permits = HELPER_LIMIT
    .acquire_many(HELPER_CAPACITY)
    .await
    .map_err(|_| errors::unavailable())?;
  Ok(())
}

/// The helper's exit status, request-write result, and bounded response bytes.
pub struct Output {
  pub success: bool,
  pub input_result: std::io::Result<()>,
  pub bytes: Vec<u8>,
}

/// Exchanges one request with a selected executable using private pipes.
///
/// Input, output, and process completion progress concurrently. Timeout and
/// caller cancellation terminate and reap the owned helper. Diagnostics are
/// discarded; the process never inherits an SSH askpass context.
///
/// # Errors
/// Returns a stable, sanitized startup, timeout, or response error.
pub async fn exchange(
  mut command: Command,
  operation: &'static str,
  request: Zeroizing<Vec<u8>>,
  maximum_output: usize,
  deadline: Duration,
) -> Result<Output, Error> {
  let deadline = tokio::time::Instant::now() + deadline;
  let permit = tokio::time::timeout_at(deadline, HELPER_LIMIT.acquire())
    .await
    .map_err(|_| errors::timeout())?
    .map_err(|_| errors::unavailable())?;
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
    .map_err(|_| errors::unavailable())?;

  // Dropping the caller also drops the sender. The supervisor keeps ownership
  // of the child long enough to kill and wait, rather than only sending a kill
  // signal from Child::drop and leaving cleanup to a later runtime poll.
  let (cancel, mut cancelled) = oneshot::channel::<()>();
  let mut supervisor = tokio::spawn(async move {
    let _permit = permit;
    let result = tokio::select! {
      result = collect(&mut child, request, maximum_output) => result,
      _ = &mut cancelled => Err(errors::unavailable()),
    };
    if result.is_err() {
      let _ = child.kill().await;
    }
    result
  });
  if let Ok(result) = tokio::time::timeout_at(deadline, &mut supervisor).await {
    result.map_err(|_| errors::unavailable())?
  } else {
    let _ = cancel.send(());
    let _ = supervisor.await;
    Err(errors::timeout())
  }
}

async fn collect(
  child: &mut Child,
  request: Zeroizing<Vec<u8>>,
  maximum_output: usize,
) -> Result<Output, Error> {
  let stdin = child.stdin.take().ok_or_else(errors::unavailable)?;
  let stdout = child.stdout.take().ok_or_else(errors::unavailable)?;
  let (status, input_result, bytes) = tokio::try_join!(
    child.wait(),
    async move {
      // Preserve a legacy helper's structured rejection even if it exits
      // before reading the entire request.
      Ok::<_, std::io::Error>(write_request(stdin, &request).await)
    },
    read_limited(stdout, maximum_output)
  )
  .map_err(|_| errors::invalid_response())?;
  Ok(Output {
    success: status.success(),
    input_result,
    bytes,
  })
}

async fn write_request(mut stdin: impl AsyncWrite + Unpin, bytes: &[u8]) -> std::io::Result<()> {
  stdin.write_all(bytes).await?;
  stdin.shutdown().await?;
  // EOF-reading helpers must see the pipe close before producing a reply.
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
