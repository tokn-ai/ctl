use super::InstallationStatus;
use super::show::{InstallationShow, MAX_SHOW_BYTES};
use crate::{CoreError, SshConnectionOptions, SshInteraction};
use ctl_core::installations::CatalogEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::process::{Output, Stdio};

const CATALOG_MARKER: &[u8] = b"ctlup-catalog-v1\n";
const SHOW_MARKER: &[u8] = b"ctlup-show-v1\n";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationCatalog {
  pub schema_version: u16,
  pub installations: Vec<CatalogEntry>,
}

impl InstallationCatalog {
  #[must_use]
  pub fn new(installations: Vec<CatalogEntry>) -> Self {
    Self {
      schema_version: 1,
      installations,
    }
  }

  /// Validates bounded metadata returned by the remote installation manager.
  ///
  /// # Errors
  /// Reports unsupported schemas, malformed entries and duplicate selections.
  pub fn validate(&self) -> io::Result<()> {
    if self.schema_version != 1 || self.installations.len() > 4096 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid installation catalog schema or size",
      ));
    }
    let mut entries = BTreeSet::new();
    let mut current = BTreeSet::new();
    for entry in &self.installations {
      entry.validate()?;
      let channel = entry.origin.as_ref().map(|origin| origin.channel.as_str());
      let target = entry.target_triple.as_str();
      if !entries.insert((channel, target, entry.bundle_id.as_str()))
        || (entry.current && !current.insert((channel, target)))
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "duplicate installation catalog entry or channel selection",
        ));
      }
    }
    Ok(())
  }
}

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

/// Reads the remote manager's verified catalog without starting services.
///
/// Older managers and absent Windows managers return `None` because they do not
/// expose this optional command. Existing status and rollback use their original
/// response independently.
///
/// # Errors
/// Reports SSH, account verification, storage and invalid catalog failures.
pub async fn remote_catalog(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<Option<InstallationCatalog>, CoreError> {
  let output = execute(
    destination,
    options,
    interaction,
    "catalog-json",
    expected_account,
  )
  .await?;
  parse_catalog(output.status.code(), &output.stdout, &output.stderr)
}

/// Reads detailed installed build identities without starting services.
/// Older managers return `None`; malformed responses and account failures do
/// not hide behind the optional command's compatibility fallback.
///
/// # Errors
/// Reports SSH, account verification, storage and invalid inventory failures.
pub async fn remote_show(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<Option<InstallationShow>, CoreError> {
  let output = execute(destination, options, interaction, "show", expected_account).await?;
  parse_show(output.status.code(), &output.stdout, &output.stderr)
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
  let output = execute(
    destination,
    options,
    interaction,
    operation,
    expected_account,
  )
  .await?;
  if !output.status.success() {
    return Err(command_error(&output.stderr));
  }
  parse_status(&output.stdout)
}

async fn execute(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  operation: &str,
  expected_account: Option<&str>,
) -> Result<Output, CoreError> {
  crate::validate_ssh_target(destination, options)?;
  let marker = match operation {
    "catalog-json" => "ctlup-catalog-v1",
    "show" => "ctlup-show-v1",
    _ => "ctlup-command-v1",
  };
  let arguments = if operation == "catalog-json" {
    operation.to_owned()
  } else {
    format!("{operation} --json")
  };
  let mut command = match options.remote_platform {
    crate::RemotePlatform::Unix => {
      let mut command = tokio::process::Command::new(crate::SSH_PROGRAM);
      let extra = crate::configure_ssh_interaction(&mut command, interaction);
      let guard = crate::ssh_install::unix_account_guard(expected_account)?;
      let script = format!(
        "{guard}test -x \"$HOME/.tokn/ctl/bin/ctlup\" || {{ printf 'ctlup is not installed; run ctlup update for this host first\\n' >&2; exit 127; }}; printf '{marker}\\n'; exec \"$HOME/.tokn/ctl/bin/ctlup\" {arguments}"
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
  if ('{operation}' -in @('catalog-json', 'show')) {{ [Console]::Out.Write("{marker}`n"); exit 2 }}
  if ('{operation}' -ne 'status') {{ throw 'ctlup is not installed for this account.' }}
  [Console]::Out.Write("ctlup-command-v1`n")
  [Console]::Out.Write('{{"active":null,"previous":null}}')
  exit 0
}}
[Console]::Out.Write("{marker}`n")
& $manager {arguments}
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
  let maximum = if operation == "show" {
    MAX_SHOW_BYTES + 8192
  } else {
    1024 * 1024
  };
  let (status, stdout, stderr) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
    tokio::try_join!(child.wait(), read(stdout, maximum), read(stderr, 8192))
  })
  .await
  .map_err(|_| {
    CoreError::InvalidComponentBundle(
      "remote ctlup operation timed out; inspect the host before retrying".into(),
    )
  })?
  .map_err(failure)?;
  Ok(Output {
    status,
    stdout,
    stderr,
  })
}

fn command_error(stderr: &[u8]) -> CoreError {
  CoreError::InvalidComponentBundle(String::from_utf8_lossy(stderr).trim().to_owned())
}

fn parse_catalog(
  code: Option<i32>,
  stdout: &[u8],
  stderr: &[u8],
) -> Result<Option<InstallationCatalog>, CoreError> {
  let start = stdout
    .windows(CATALOG_MARKER.len())
    .position(|window| window == CATALOG_MARKER)
    .map(|start| start + CATALOG_MARKER.len());
  // Fixed valid arguments make clap's usage-error exit an explicit capability
  // result. The wrapper marker proves account verification completed first.
  if code == Some(2) && start.is_some() {
    return Ok(None);
  }
  if code != Some(0) {
    return Err(command_error(stderr));
  }
  let start = start
    .ok_or_else(|| CoreError::InvalidComponentBundle("missing ctlup catalog response".into()))?;
  let catalog: InstallationCatalog = serde_json::from_slice(&stdout[start..])
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  catalog
    .validate()
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  Ok(Some(catalog))
}

fn parse_show(
  code: Option<i32>,
  stdout: &[u8],
  stderr: &[u8],
) -> Result<Option<InstallationShow>, CoreError> {
  let start = stdout
    .windows(SHOW_MARKER.len())
    .position(|window| window == SHOW_MARKER)
    .map(|start| start + SHOW_MARKER.len());
  if code == Some(2) && start.is_some() {
    return Ok(None);
  }
  if code != Some(0) {
    return Err(command_error(stderr));
  }
  let start =
    start.ok_or_else(|| CoreError::InvalidComponentBundle("missing ctlup show response".into()))?;
  if stdout[start..].len() > MAX_SHOW_BYTES {
    return Err(CoreError::InvalidComponentBundle(
      "ctlup show response exceeds its size limit".into(),
    ));
  }
  let snapshot: InstallationShow = serde_json::from_slice(&stdout[start..])
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  snapshot
    .validate()
    .map_err(|error| CoreError::InvalidComponentBundle(error.to_string()))?;
  Ok(Some(snapshot))
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
  use ctl_core::installations::CatalogOrigin;

  fn entry(channel: &str, id: char) -> CatalogEntry {
    CatalogEntry {
      bundle_id: id.to_string().repeat(64),
      target_triple: "x86_64-unknown-linux-musl".into(),
      origin: Some(CatalogOrigin {
        channel: channel.into(),
        publication_id: None,
        built_at: None,
      }),
      current: true,
    }
  }

  #[test]
  fn catalog_response_is_versioned_and_preserves_unknown_legacy_metadata() {
    let mut legacy = entry("nightly", 'a');
    legacy.origin = None;
    legacy.current = false;
    let catalog = InstallationCatalog::new(vec![legacy]);
    let mut stdout = b"Welcome\nctlup-catalog-v1\n".to_vec();
    stdout.extend(serde_json::to_vec(&catalog).unwrap());
    let parsed = parse_catalog(Some(0), &stdout, b"").unwrap().unwrap();
    assert_eq!(parsed.installations, catalog.installations);

    assert!(parse_catalog(Some(0), b"{}", b"").is_err());
    assert!(
      parse_catalog(
        Some(0),
        b"ctlup-catalog-v1\n{\"schema_version\":2,\"installations\":[]}",
        b""
      )
      .is_err()
    );
    assert!(parse_catalog(Some(0), b"ctlup-catalog-v1\nnot json", b"").is_err());
  }

  #[test]
  fn unavailable_catalog_capability_requires_the_guarded_wrapper_marker() {
    assert!(
      parse_catalog(Some(2), CATALOG_MARKER, b"older manager")
        .unwrap()
        .is_none()
    );
    for (code, stdout) in [
      (Some(2), b"".as_slice()),
      (Some(1), CATALOG_MARKER),
      (Some(255), CATALOG_MARKER),
    ] {
      let error = parse_catalog(code, stdout, b"account verification failed")
        .unwrap_err()
        .to_string();
      assert!(error.contains("account verification failed"));
    }
  }

  #[test]
  fn show_response_uses_its_own_optional_capability_and_schema() {
    let snapshot = InstallationShow {
      schema_version: 1,
      active: None,
      previous: None,
      installations: Vec::new(),
      builds: Vec::new(),
    };
    let mut stdout = b"Welcome\nctlup-show-v1\n".to_vec();
    stdout.extend(serde_json::to_vec(&snapshot).unwrap());
    let result = parse_show(Some(0), &stdout, b"").unwrap().unwrap();
    assert_eq!(result.builds, [] as [super::super::InstalledBuild; 0]);
    assert!(
      parse_show(Some(2), SHOW_MARKER, b"older manager")
        .unwrap()
        .is_none()
    );
    for (code, stdout) in [
      (Some(2), b"".as_slice()),
      (Some(2), CATALOG_MARKER),
      (Some(1), SHOW_MARKER),
      (Some(255), SHOW_MARKER),
    ] {
      let error = parse_show(code, stdout, b"account verification failed")
        .unwrap_err()
        .to_string();
      assert!(error.contains("account verification failed"));
    }
    assert!(parse_show(Some(0), CATALOG_MARKER, b"").is_err());
    let mut invalid = snapshot;
    invalid.schema_version = 2;
    let mut stdout = SHOW_MARKER.to_vec();
    stdout.extend(serde_json::to_vec(&invalid).unwrap());
    assert!(parse_show(Some(0), &stdout, b"").is_err());
    assert!(parse_show(Some(0), b"ctlup-show-v1\nnot json", b"").is_err());
  }

  #[test]
  fn show_payload_limit_is_independent_of_wrapper_output() {
    let mut stdout = SHOW_MARKER.to_vec();
    stdout.resize(SHOW_MARKER.len() + MAX_SHOW_BYTES + 1, b' ');
    let error = parse_show(Some(0), &stdout, b"").unwrap_err().to_string();
    assert!(error.contains("size limit"));
  }

  #[test]
  fn catalog_rejects_duplicate_entries_and_conflicting_channel_heads() {
    let first = entry("nightly", 'a');
    let other_channel = entry("stable", 'a');
    InstallationCatalog::new(vec![first.clone(), other_channel])
      .validate()
      .unwrap();
    assert!(
      InstallationCatalog::new(vec![first.clone(), first.clone()])
        .validate()
        .is_err()
    );
    let mut older = entry("nightly", 'b');
    assert!(
      InstallationCatalog::new(vec![first.clone(), older.clone()])
        .validate()
        .is_err()
    );
    older.current = false;
    InstallationCatalog::new(vec![first, older])
      .validate()
      .unwrap();
  }

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
