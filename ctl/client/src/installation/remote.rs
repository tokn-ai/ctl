use super::InstallationStatus;
use crate::{CoreError, SshConnectionOptions, SshInteraction};
use std::process::Stdio;

/// Inspect the remote account's active installation without bootstrapping services.
///
/// # Errors
/// Reports SSH, missing updater, or invalid installation metadata.
pub async fn remote_status(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<InstallationStatus, CoreError> {
  run(
    destination,
    options,
    interaction,
    "status",
    expected_account,
  )
  .await
}

/// Atomically restore the remote account's previous build, preserving services.
/// Callers must verify saved account identity before issuing this mutation.
///
/// # Errors
/// Reports SSH, unavailable rollback or uncertain activation results; never retries.
pub async fn remote_rollback(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<InstallationStatus, CoreError> {
  run(
    destination,
    options,
    interaction,
    "rollback",
    expected_account,
  )
  .await
}

/// Reads the selected SSH account's existing identity without starting services.
///
/// # Errors
/// Reports SSH failures and missing or invalid account identities.
pub async fn remote_account_id(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<String, CoreError> {
  match options.remote_platform {
    crate::RemotePlatform::Unix => {
      crate::maintenance::inspect_remote_id_interactive(destination, options, interaction).await
    }
    crate::RemotePlatform::Windows => {
      crate::ssh_install::read_remote_id(destination, options, interaction).await
    }
  }
}

async fn run(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  operation: &str,
  expected_account: Option<&str>,
) -> Result<InstallationStatus, CoreError> {
  crate::validate_ssh_target(destination, options)?;
  let mut command = match options.remote_platform {
    crate::RemotePlatform::Unix => {
      let mut command = tokio::process::Command::new(crate::SSH_PROGRAM);
      let extra = crate::configure_ssh_interaction(&mut command, interaction);
      let guard = crate::ssh_install::unix_account_guard(expected_account)?;
      let script = format!(
        "{guard}test -x \"$HOME/.tokn/ctl/bin/ctlup\" || {{ printf 'ctlup is not installed; run ctlup update for this host first\\n' >&2; exit 127; }}; printf 'ctlup-command-v1\\n'; exec \"$HOME/.tokn/ctl/bin/ctlup\" {operation} --json"
      );
      command
        .args(extra)
        .args(crate::prepare_ssh_base_arguments(destination, options, interaction).await?)
        .arg(format!("exec sh -c '{}'", script.replace('\'', "'\\''")));
      command
    }
    crate::RemotePlatform::Windows => {
      let guard = crate::ssh_install::windows_account_guard(expected_account)?;
      let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
{guard}
$manager = Join-Path $env:USERPROFILE '.tokn\ctl\bin\ctlup.exe'
if (![IO.File]::Exists($manager)) {{
  if ('{operation}' -ne 'status') {{ throw 'ctlup is not installed for this account.' }}
  [Console]::Out.Write("ctlup-command-v1`n")
  [Console]::Out.Write('{{"active":null,"previous":null}}')
  exit 0
}}
[Console]::Out.Write("ctlup-command-v1`n")
& $manager {operation} --json
exit $LASTEXITCODE
"#
      );
      crate::ssh_install::windows_ssh_command(destination, options, interaction, &script).await?
    }
  };
  command
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let failure = |error: std::io::Error| CoreError::InvalidComponentBundle(error.to_string());
  let mut child = command.spawn().map_err(failure)?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| failure(std::io::Error::other("missing ctlup output")))?;
  let stderr = child
    .stderr
    .take()
    .ok_or_else(|| failure(std::io::Error::other("missing ctlup diagnostics")))?;
  let (status, stdout, stderr) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
    tokio::try_join!(child.wait(), read(stdout, 1024 * 1024), read(stderr, 8192))
  })
  .await
  .map_err(|_| {
    CoreError::InvalidComponentBundle(
      "remote ctlup operation timed out; inspect the host before retrying".into(),
    )
  })?
  .map_err(failure)?;
  if !status.success() {
    return Err(CoreError::InvalidComponentBundle(
      String::from_utf8_lossy(&stderr).trim().to_owned(),
    ));
  }
  parse_status(&stdout)
}

fn parse_status(stdout: &[u8]) -> Result<InstallationStatus, CoreError> {
  let marker = b"ctlup-command-v1\n";
  let start = stdout
    .windows(marker.len())
    .position(|window| window == marker)
    .map(|start| start + marker.len())
    .ok_or_else(|| CoreError::InvalidComponentBundle("missing ctlup command response".into()))?;
  let snapshot: InstallationStatus = serde_json::from_slice(&stdout[start..])
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  for manifest in [snapshot.active.as_ref(), snapshot.previous.as_ref()]
    .into_iter()
    .flatten()
  {
    manifest
      .validate()
      .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  }
  Ok(snapshot)
}

async fn read(
  reader: impl tokio::io::AsyncRead + Unpin,
  maximum: usize,
) -> std::io::Result<Vec<u8>> {
  use tokio::io::AsyncReadExt as _;
  let mut bytes = Vec::new();
  reader
    .take(maximum as u64 + 1)
    .read_to_end(&mut bytes)
    .await?;
  if bytes.len() > maximum {
    return Err(std::io::Error::other("ctlup output exceeds its size limit"));
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn status_uses_its_marker_after_shell_profile_output() {
    let status = parse_status(
      b"Welcome to the fixture account\nctlup-command-v1\n{\"active\":null,\"previous\":null}\n",
    )
    .unwrap();
    assert!(status.active.is_none());
    assert!(status.previous.is_none());
    assert!(parse_status(b"{\"active\":null,\"previous\":null}").is_err());
    assert!(
      parse_status(b"ctlup-command-v1\n{\"active\":null,\"previous\":null,\"untrusted\":true}")
        .is_err()
    );
  }
}
