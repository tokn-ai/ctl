//! Read the account pin independently of the agent being replaced.

use std::{path::Path, time::Duration};

use tokio::process::Command;

use crate::{CoreError, SshConnectionOptions};

const MARKER: &[u8] = b"ctl-command-v1\n";
const READ_SCRIPT: &str = r#"set -eu
printf 'ctl-command-v1\n'
identity="$HOME/.tokn/ctl/remote-id"
if ! test -f "$identity"; then
  printf 'The saved ctl account identity is missing or is not a regular file.\n' >&2
  exit 1
fi
exec dd if="$identity" bs=1 count=37
"#;

/// Reads an existing account UUID without invoking an agent or starting services.
///
/// Only the selected authenticated master may be reused. Missing or malformed
/// identity files are never created, repaired, or replaced by this inspection.
///
/// # Errors
/// Returns errors for an unavailable master, missing identity, or invalid UUID.
pub async fn inspect_remote_id(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
) -> Result<String, CoreError> {
  inspect(command(destination, options, control_path)?).await
}

fn command(
  destination: &str,
  options: &SshConnectionOptions,
  control_path: &Path,
) -> Result<Command, CoreError> {
  // The login shell only parses this launcher. The fixed reader runs in POSIX
  // sh even for accounts whose login shell is fish or tcsh.
  super::command(destination, options, control_path, &remote_command())
}

fn remote_command() -> String {
  format!("exec sh -c '{}'", READ_SCRIPT.replace('\'', "'\\''"))
}

async fn inspect(command: Command) -> Result<String, CoreError> {
  let output = tokio::time::timeout(
    Duration::from_secs(8),
    crate::run_marked_fixed_command(command, &[], MARKER),
  )
  .await
  .map_err(|_| CoreError::InvalidSshCommandOutput)??;
  let remote_id = String::from_utf8(output).map_err(|_| CoreError::InvalidSshCommandOutput)?;
  // Match the agent's exact on-disk format: canonical, non-nil UUID without
  // trailing whitespace. Reading one extra byte rejects oversized files.
  if !ctl_proto::valid_remote_id(&remote_id) {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  Ok(remote_id)
}

#[cfg(all(test, unix))]
mod tests;
