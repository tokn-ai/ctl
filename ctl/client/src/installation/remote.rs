use super::show::{InstallationShow, MAX_SHOW_BYTES};
use super::{InstallationStatus, RuntimeSource};
use crate::{CoreError, SshConnectionOptions, SshInteraction};
use ctl_core::installations::CatalogEntry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::process::{Output, Stdio};

const CATALOG_MARKER: &[u8] = b"ctlup-catalog-v1\n";
const SHOW_MARKER: &[u8] = b"ctlup-show-v2\n";
const COMMAND_MARKER: &[u8] = b"ctlup-command-v1\n";
const SOURCE_MARKER: &[u8] = b"ctlup-runtime-source-v1\n";

#[derive(Debug)]
enum StatusResponse {
  Inventory(InstallationShow),
  Legacy(InstallationStatus),
}

#[derive(Deserialize)]
struct StatusVersion {
  schema_version: Option<u16>,
}

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
  let output = execute(
    destination,
    options,
    interaction,
    "inspect-runtime",
    expected_account,
  )
  .await?;
  if let Some(bytes) = response(
    output.status.code(),
    &output.stdout,
    &output.stderr,
    COMMAND_MARKER,
  )? {
    return parse_status(bytes);
  }
  // ctlup before layout 7 exposed this manifest response as "status --json".
  if let Some(StatusResponse::Legacy(status)) =
    query_status(destination, options, interaction, expected_account).await?
  {
    return Ok(status);
  }
  Err(invalid_response(
    "the remote ctlup does not expose runtime manifests",
  ))
}

/// Reads the runtime source stored by the selected SSH account.
///
/// # Errors
/// Reports failed SSH operations, invalid source metadata or identity mismatch.
pub async fn remote_runtime_source(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<RuntimeSource, CoreError> {
  let output = execute(
    destination,
    options,
    interaction,
    "runtime-source",
    expected_account,
  )
  .await?;
  if let Some(bytes) = response(
    output.status.code(),
    &output.stdout,
    &output.stderr,
    SOURCE_MARKER,
  )? {
    let source: RuntimeSource = serde_json::from_slice(bytes).map_err(invalid_response)?;
    source.validate().map_err(invalid_response)?;
    return Ok(source);
  }
  let inventory = remote_show(destination, options, interaction, expected_account)
    .await?
    .ok_or_else(unknown_source)?;
  let active = inventory.active;
  if active.is_some() && inventory.active_origin.is_none() {
    return Err(unknown_source());
  }
  let build = active
    .as_ref()
    .and_then(|id| inventory.builds.iter().find(|build| &build.bundle_id == id));
  let missing = active
    .as_ref()
    .is_some_and(|id| inventory.missing_builds.contains(id));
  let target_triple = build.map(|build| build.target_triple.clone()).or_else(|| {
    let id = active.as_ref()?;
    inventory
      .installations
      .iter()
      .find(|entry| &entry.bundle_id == id)
      .map(|entry| entry.target_triple.clone())
  });
  Ok(RuntimeSource {
    active,
    missing,
    origin: inventory.active_origin,
    target_triple,
  })
}

fn unknown_source() -> CoreError {
  CoreError::InvalidInstallationResponse(
    "The remote ctlup cannot identify the active runtime's source. In About, use Install from… to choose a source, or run ctlup install with an explicit source for this host.".into(),
  )
}

/// Confirms that the remote updater activated the verified installation archive.
/// # Errors
/// Reports SSH failures, invalid status or an active build that differs from the archive.
pub async fn verify_remote_activation(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
  expected: &ctl_core::bundles::Manifest,
) -> io::Result<()> {
  let status = remote_status(destination, options, interaction, expected_account)
    .await
    .map_err(|error| {
      io::Error::other(format!(
        "Components installed, but activation could not be verified. Refresh status before retrying: {error}"
      ))
    })?;
  if status.active.as_ref() != Some(expected) {
    return Err(io::Error::other(
      "Components installed, but activation could not be verified. Refresh status before retrying.",
    ));
  }
  Ok(())
}

/// Reads the remote manager's verified catalog without starting services.
///
/// Older managers and absent Windows managers return `None` because they do not
/// expose this optional command. Status uses its original response independently.
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
/// Returns `None` when the authenticated manager reports that the command is
/// unavailable. Older manifest responses are combined with their catalog.
///
/// # Errors
/// Reports SSH, account verification, storage and invalid inventory failures.
pub async fn remote_show(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<Option<InstallationShow>, CoreError> {
  match query_status(destination, options, interaction, expected_account).await? {
    None => Ok(None),
    Some(StatusResponse::Inventory(snapshot)) => Ok(Some(snapshot)),
    Some(StatusResponse::Legacy(status)) => {
      let catalog = remote_catalog(destination, options, interaction, expected_account).await?;
      legacy_inventory(status, catalog)
        .map(Some)
        .map_err(invalid_response)
    }
  }
}

fn legacy_inventory(
  status: InstallationStatus,
  mut catalog: Option<InstallationCatalog>,
) -> io::Result<InstallationShow> {
  // A channel head describes catalog membership, not which channel activated
  // the payload. A unique recorded membership is the only available evidence.
  let mut origin = None;
  if let Some(catalog) = &mut catalog {
    catalog.validate()?;
    let mut targets = [&status.active, &status.previous]
      .into_iter()
      .flatten()
      .map(|manifest| (manifest.bundle_id.as_str(), manifest.target_triple.as_str()))
      .collect::<BTreeMap<_, _>>();
    for entry in &catalog.installations {
      if targets
        .insert(&entry.bundle_id, &entry.target_triple)
        .is_some_and(|target| target != entry.target_triple)
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "installed build and catalog targets differ",
        ));
      }
    }
    if let Some(active) = &status.active {
      let mut matches = catalog
        .installations
        .iter()
        .filter(|entry| entry.bundle_id == active.bundle_id);
      if let (Some(entry), None) = (matches.next(), matches.next()) {
        origin.clone_from(&entry.origin);
      }
    }
    // Catalog v1 can list unidentified payloads. Inventory v2 represents
    // catalog memberships only when a source was recorded.
    catalog.installations.retain(|entry| entry.origin.is_some());
  }
  let mut snapshot = InstallationShow::from_status(status, catalog)?;
  snapshot.active_origin = origin;
  snapshot.validate()?;
  Ok(snapshot)
}

async fn query_status(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_account: Option<&str>,
) -> Result<Option<StatusResponse>, CoreError> {
  let output = execute(
    destination,
    options,
    interaction,
    "status",
    expected_account,
  )
  .await?;
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
    "status" => "ctlup-show-v2",
    "runtime-source" => "ctlup-runtime-source-v1",
    _ => "ctlup-command-v1",
  };
  let arguments = if matches!(operation, "catalog-json" | "runtime-source") {
    operation.to_owned()
  } else {
    format!("{operation} --json")
  };
  let mut command = match options.remote_platform {
    crate::RemotePlatform::Unix => {
      let mut command = tokio::process::Command::new(crate::SSH_PROGRAM);
      let extra = crate::configure_ssh_interaction(&mut command, interaction);
      let guard = crate::ssh_install::unix_account_guard(expected_account)?;
      let missing = if operation == "runtime-source" {
        format!("printf '%s\\n' '{marker}' '{{\"active\":null,\"missing\":false}}'; exit 0;")
      } else {
        "printf 'ctlup is not installed; run ctlup install for this host first\\n' >&2; exit 127;"
          .into()
      };
      let script = format!(
        "{guard}test -x \"$HOME/.tokn/ctl/bin/ctlup\" || {{ {missing} }}; printf '{marker}\\n'; exec \"$HOME/.tokn/ctl/bin/ctlup\" {arguments}"
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
  if ('{operation}' -in @('catalog-json', 'status')) {{ [Console]::Out.Write("{marker}`n"); exit 2 }}
  if ('{operation}' -eq 'runtime-source') {{
    [Console]::Out.Write("{marker}`n")
    [Console]::Out.Write('{{"active":null,"missing":false}}')
    exit 0
  }}
  if ('{operation}' -ne 'inspect-runtime') {{ throw 'ctlup is not installed for this account.' }}
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
  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let maximum = if operation == "status" {
    MAX_SHOW_BYTES + 8192
  } else {
    1024 * 1024
  };
  let (status, stdout, stderr) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
    tokio::try_join!(
      async { child.wait().await.map_err(CoreError::WaitSshCommand) },
      async {
        read(stdout, maximum)
          .await
          .map_err(CoreError::ReadSshCommand)
      },
      async { read(stderr, 8192).await.map_err(CoreError::ReadSshCommand) },
    )
  })
  .await
  .map_err(|_| {
    invalid_response("The remote ctlup operation timed out. Refresh status before retrying.")
  })??;
  Ok(Output {
    status,
    stdout,
    stderr,
  })
}

fn invalid_response(error: impl std::fmt::Display) -> CoreError {
  CoreError::InvalidInstallationResponse(format!("Remote ctlup inspection failed: {error}"))
}

fn response<'a>(
  code: Option<i32>,
  stdout: &'a [u8],
  stderr: &[u8],
  marker: &[u8],
) -> Result<Option<&'a [u8]>, CoreError> {
  let start = stdout
    .windows(marker.len())
    .position(|window| window == marker)
    .map(|start| start + marker.len());
  // Fixed valid arguments make clap's usage-error exit an explicit capability
  // result. The wrapper marker proves account verification completed first.
  if code == Some(2) && start.is_some_and(|start| stdout[start..].trim_ascii().is_empty()) {
    return Ok(None);
  }
  if code != Some(0) {
    return Err(CoreError::SshCommandFailed {
      status: code.map_or_else(
        || "no exit code".into(),
        |code| format!("exit status {code}"),
      ),
      diagnostic: String::from_utf8_lossy(stderr).trim().to_owned(),
    });
  }
  start
    .map(|start| Some(&stdout[start..]))
    .ok_or_else(|| invalid_response("missing response marker"))
}

fn parse_catalog(
  code: Option<i32>,
  stdout: &[u8],
  stderr: &[u8],
) -> Result<Option<InstallationCatalog>, CoreError> {
  let Some(bytes) = response(code, stdout, stderr, CATALOG_MARKER)? else {
    return Ok(None);
  };
  let catalog: InstallationCatalog = serde_json::from_slice(bytes).map_err(invalid_response)?;
  catalog.validate().map_err(invalid_response)?;
  Ok(Some(catalog))
}

fn parse_show(
  code: Option<i32>,
  stdout: &[u8],
  stderr: &[u8],
) -> Result<Option<StatusResponse>, CoreError> {
  let Some(bytes) = response(code, stdout, stderr, SHOW_MARKER)? else {
    return Ok(None);
  };
  if bytes.len() > MAX_SHOW_BYTES {
    return Err(invalid_response(
      "runtime status response exceeds its size limit",
    ));
  }
  let version: StatusVersion = serde_json::from_slice(bytes).map_err(invalid_response)?;
  let response = if version.schema_version.is_some() {
    let snapshot: InstallationShow = serde_json::from_slice(bytes).map_err(invalid_response)?;
    snapshot.validate().map_err(invalid_response)?;
    StatusResponse::Inventory(snapshot)
  } else {
    StatusResponse::Legacy(parse_status(bytes)?)
  };
  Ok(Some(response))
}

fn parse_status(bytes: &[u8]) -> Result<InstallationStatus, CoreError> {
  let snapshot: InstallationStatus = serde_json::from_slice(bytes).map_err(invalid_response)?;
  snapshot.validate().map_err(invalid_response)?;
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
      schema_version: 2,
      active_origin: None,
      active: None,
      previous: None,
      installations: Vec::new(),
      builds: Vec::new(),
      missing_builds: Vec::new(),
    };
    let mut stdout = b"Welcome\nctlup-show-v2\n".to_vec();
    stdout.extend(serde_json::to_vec(&snapshot).unwrap());
    let Some(StatusResponse::Inventory(result)) = parse_show(Some(0), &stdout, b"").unwrap() else {
      panic!("expected the current inventory schema");
    };
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
    invalid.schema_version = 1;
    let mut stdout = SHOW_MARKER.to_vec();
    stdout.extend(serde_json::to_vec(&invalid).unwrap());
    assert!(parse_show(Some(0), &stdout, b"").is_err());
    for body in [
      b"not json".as_slice(),
      b"{}",
      b"{\"active\":null}",
      b"{\"active\":null,\"previous\":null,\"schema_version\":null}",
    ] {
      let mut stdout = SHOW_MARKER.to_vec();
      stdout.extend(body);
      assert!(parse_show(Some(0), &stdout, b"").is_err());
    }
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
    let extract = |bytes| response(Some(0), bytes, b"", COMMAND_MARKER);
    let status = parse_status(
      extract(
        b"Welcome to the fixture account\nctlup-command-v1\n{\"active\":null,\"previous\":null}\n",
      )
      .unwrap()
      .unwrap(),
    )
    .unwrap();
    assert!(status.active.is_none());
    assert!(status.previous.is_none());
    assert_eq!(status.missing_builds, [] as [String; 0]);
    assert!(extract(b"{\"active\":null,\"previous\":null}").is_err());
    assert!(parse_status(b"{\"active\":null,\"previous\":null,\"untrusted\":true}").is_err());
  }

  #[test]
  fn status_accepts_bounded_missing_build_ids() {
    let id = "a".repeat(64);
    let payload = |ids| {
      format!(
        "{}",
        serde_json::json!({"active": null, "previous": null, "missing_builds": ids})
      )
    };
    let status = parse_status(payload(vec![id.as_str()]).as_bytes()).unwrap();
    assert_eq!(status.missing_builds, [id.as_str()]);
    assert!(parse_status(payload(vec!["../invalid"]).as_bytes()).is_err());
    assert!(parse_status(payload(vec![id.as_str(); 2]).as_bytes()).is_err());
  }

  #[test]
  fn a_guarded_usage_error_enables_the_legacy_source_probe() {
    let diagnostic = b"error: unrecognized subcommand 'runtime-source'";
    assert!(
      response(Some(2), SOURCE_MARKER, diagnostic, SOURCE_MARKER)
        .unwrap()
        .is_none()
    );
    for (code, marker) in [
      (2, b"".as_slice()),
      (2, b"ctlup-runtime-source-v1\npartial response"),
      (1, SOURCE_MARKER),
      (255, SOURCE_MARKER),
    ] {
      assert!(matches!(
        response(Some(code), marker, b"account or SSH failure", SOURCE_MARKER),
        Err(CoreError::SshCommandFailed { .. })
      ));
    }
    assert!(response(Some(0), b"{}", b"", SOURCE_MARKER).is_err());
  }
}
