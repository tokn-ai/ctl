//! Bounded stdin/stdout exchange with the signed, one-shot credential helper.

use std::time::Duration;

use tokio::process::Command;
use zeroize::Zeroizing;

use crate::error::{CommandErrorDto, CommandResult};

pub(super) use ctl_client::local_credentials::Output;

pub(super) async fn exchange(
  command: Command,
  operation: &'static str,
  request: Zeroizing<Vec<u8>>,
  maximum_output: usize,
  deadline: Duration,
) -> CommandResult<Output> {
  ctl_client::local_credentials::exchange(command, operation, request, maximum_output, deadline)
    .await
    .map_err(|error| CommandErrorDto::new(error.code, error.message))
}

pub(super) fn preparation_error(error: ctl_ipc::ConnectError) -> CommandErrorDto {
  if matches!(error, ctl_ipc::ConnectError::PrepareDaemon(source) if source.kind() == std::io::ErrorKind::TimedOut)
  {
    CommandErrorDto::new(
      "credential_helper_timeout",
      crate::daemon_helper::TIMEOUT_MESSAGE,
    )
  } else {
    unavailable()
  }
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
