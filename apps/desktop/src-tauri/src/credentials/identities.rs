//! Identity inspection and verified passphrase writes through the signed helper.
#![allow(clippy::needless_pass_by_value)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use ctl_ipc::identities::{IdentityFile, Inventory, MAX_PATHS, Request, Response};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::models::{ListRequest, NamedTarget};
use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

#[cfg(test)]
mod tests;

#[derive(Serialize)]
pub struct IdentityRecord {
  #[serde(flatten)]
  file: IdentityFile,
  used_by: Vec<String>,
}

#[derive(Serialize)]
pub struct IdentitySnapshot {
  identity_files: Vec<IdentityRecord>,
  complete: bool,
  warning: Option<String>,
  keychain_available: bool,
  keychain_message: Option<String>,
  metadata_import_required: bool,
  checked_at_ms: i64,
}

// No Debug on secret-bearing command inputs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
  path: String,
  file_version: String,
  passphrase: Zeroizing<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgetRequest {
  identity_id: String,
}

#[derive(Default)]
struct PathHints {
  names: BTreeMap<String, BTreeSet<String>>,
  complete: bool,
}

#[tauri::command]
pub async fn list_credential_identity_files(
  request: ListRequest,
) -> CommandResult<IdentitySnapshot> {
  let hints = tauri::async_runtime::spawn_blocking(move || {
    let mut hints = host_paths(&request.targets, dirs::home_dir().as_deref());
    match crate::ssh_config::discover_hosts() {
      Ok(config) => {
        hints.complete &= config.warnings.is_empty() && config.identity_warnings.is_empty();
        for path in config.identity_paths {
          insert_path(&mut hints, &path, None, dirs::home_dir().as_deref());
        }
      }
      Err(_) => hints.complete = false,
    }
    hints
  })
  .await
  .map_err(|_| sanitized_error("identity_list_failed"))?;
  let paths = bounded_paths(&hints.names);
  let hints_complete = hints.complete && paths.len() == hints.names.len();
  let Response::Inventory { inventory } = exchange(Request::ListMetadata { paths }).await? else {
    return Err(sanitized_error("identity_list_failed"));
  };
  Ok(snapshot(inventory, &hints.names, hints_complete))
}

#[tauri::command]
pub async fn save_identity_passphrase(request: SaveRequest) -> CommandResult<()> {
  if !valid_path(&request.path)
    || !valid_digest(&request.file_version)
    || request.passphrase.is_empty()
    || request.passphrase.len() > 16 * 1024
    || request.passphrase.contains(['\0', '\n', '\r'])
  {
    return Err(sanitized_error("identity_invalid_request"));
  }
  match exchange(Request::Save {
    path: request.path,
    file_version: request.file_version,
    passphrase: request.passphrase,
  })
  .await?
  {
    Response::Saved => Ok(()),
    _ => Err(sanitized_error("identity_save_failed")),
  }
}

#[tauri::command]
pub async fn forget_identity_passphrase(request: ForgetRequest) -> CommandResult<()> {
  if !valid_digest(&request.identity_id) {
    return Err(sanitized_error("identity_invalid_request"));
  }
  match exchange(Request::Forget {
    identity_id: request.identity_id,
  })
  .await?
  {
    Response::Forgotten => Ok(()),
    _ => Err(sanitized_error("identity_forget_failed")),
  }
}

#[cfg(unix)]
async fn exchange(request: Request) -> CommandResult<Response> {
  let executable = ctl_ipc::daemon_executable().map_err(|_| super::process::unavailable())?;
  exchange_with(tokio::process::Command::new(executable), request).await
}

#[cfg(not(unix))]
fn exchange(_request: Request) -> std::future::Ready<CommandResult<Response>> {
  std::future::ready(Err(sanitized_error("identity_unsupported")))
}

#[cfg(unix)]
async fn exchange_with(
  command: tokio::process::Command,
  request: Request,
) -> CommandResult<Response> {
  use ctl_ipc::identities::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES};

  let requires_metadata_support = matches!(request, Request::ListMetadata { .. });
  let bytes = Zeroizing::new(
    serde_json::to_vec(&request).map_err(|_| sanitized_error("identity_invalid_request"))?,
  );
  drop(request);
  if bytes.len() > MAX_REQUEST_BYTES {
    return Err(sanitized_error("identity_invalid_request"));
  }
  let output = super::process::exchange(
    command,
    "--identity-request",
    bytes,
    MAX_RESPONSE_BYTES,
    std::time::Duration::from_mins(1),
  )
  .await?;
  let response = serde_json::from_slice::<Response>(&output.bytes).map_err(|_| {
    if output.success {
      super::process::invalid_response()
    } else {
      super::process::unsupported()
    }
  })?;
  if let Response::Error { code, .. } = response {
    if requires_metadata_support && code == "identity_invalid_request" {
      return Err(super::process::unsupported());
    }
    return Err(sanitized_error(&code));
  }
  if !output.success || output.input_result.is_err() {
    return Err(super::process::invalid_response());
  }
  Ok(response)
}

fn host_paths(targets: &[NamedTarget], home: Option<&Path>) -> PathHints {
  let mut hints = PathHints {
    complete: targets.len() <= MAX_PATHS,
    ..PathHints::default()
  };
  for named in targets.iter().take(MAX_PATHS) {
    let name = if super::valid_text(&named.name, 256, true) {
      Some(named.name.as_str())
    } else {
      hints.complete = false;
      None
    };
    if let ConnectionTargetDto::Ssh {
      identity_file,
      gateways,
      destination,
      hostname,
      ssh_config_alias,
      user,
      port,
      ..
    } = &named.target
    {
      if let Some(path) = identity_file {
        insert_configured_path(
          &mut hints,
          path,
          name,
          home,
          IdentityContext {
            original_host: ssh_config_alias
              .as_deref()
              .or(hostname.as_deref())
              .unwrap_or(destination),
            hostname: hostname.as_deref(),
            user: user.as_deref(),
            port: *port,
          },
        );
      }
      hints.complete &= gateways.len() <= 8;
      for gateway in gateways.iter().take(8) {
        if let Some(path) = &gateway.identity_file {
          insert_configured_path(
            &mut hints,
            path,
            name,
            home,
            IdentityContext {
              original_host: gateway.hostname.as_deref().unwrap_or(&gateway.destination),
              hostname: gateway.hostname.as_deref(),
              user: gateway.user.as_deref(),
              port: gateway.port,
            },
          );
        }
      }
    }
  }
  hints
}

struct IdentityContext<'a> {
  original_host: &'a str,
  hostname: Option<&'a str>,
  user: Option<&'a str>,
  port: Option<u16>,
}

fn insert_configured_path(
  hints: &mut PathHints,
  value: &str,
  name: Option<&str>,
  home: Option<&Path>,
  context: IdentityContext<'_>,
) {
  let Some(expanded) = expand_identity_tokens(value, home, context) else {
    hints.complete = false;
    return;
  };
  insert_path(hints, &expanded, name, home);
}

fn expand_identity_tokens(
  value: &str,
  home: Option<&Path>,
  context: IdentityContext<'_>,
) -> Option<String> {
  if value.contains("${") {
    return None;
  }
  let mut result = String::new();
  let mut characters = value.chars();
  while let Some(character) = characters.next() {
    if character != '%' {
      result.push(character);
      continue;
    }
    match characters.next()? {
      '%' => result.push('%'),
      'd' => result.push_str(home?.to_str()?),
      'h' => result.push_str(context.hostname?),
      'r' => result.push_str(context.user?),
      'p' => result.push_str(&context.port?.to_string()),
      // %n is the original host argument, without an optional user prefix.
      'n' => result.push_str(
        context
          .original_host
          .rsplit_once('@')
          .map_or(context.original_host, |(_, host)| host),
      ),
      _ => return None,
    }
  }
  Some(result)
}

fn insert_path(hints: &mut PathHints, value: &str, name: Option<&str>, home: Option<&Path>) {
  if value.trim().is_empty() || value.eq_ignore_ascii_case("none") {
    return;
  }
  let Some(path) = normalize_path(value, home) else {
    hints.complete = false;
    return;
  };
  if hints.names.len() >= MAX_PATHS && !hints.names.contains_key(&path) {
    hints.complete = false;
    return;
  }
  let names = hints.names.entry(path).or_default();
  if let Some(name) = name {
    names.insert(name.to_owned());
  }
}

fn valid_path(value: &str) -> bool {
  !value.trim().is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

fn normalize_path(value: &str, home: Option<&Path>) -> Option<String> {
  if !valid_path(value) || value.eq_ignore_ascii_case("none") {
    return None;
  }
  let path = if let Some(relative) = value.strip_prefix("~/") {
    home?.join(relative)
  } else if value.starts_with('~') {
    return None;
  } else {
    PathBuf::from(value)
  };
  let path = if path.is_absolute() {
    path
  } else {
    std::env::current_dir().ok()?.join(path)
  };
  let path = std::fs::canonicalize(&path).unwrap_or_else(|_| {
    // Match the helper's identity when the leaf is missing but its directory
    // resolves through an alias or symlink (for example /tmp on macOS).
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
      && let Ok(parent) = std::fs::canonicalize(parent)
    {
      return parent.join(name);
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
      match component {
        Component::CurDir => {}
        Component::ParentDir => {
          normalized.pop();
        }
        component => normalized.push(component.as_os_str()),
      }
    }
    normalized
  });
  path
    .into_os_string()
    .into_string()
    .ok()
    .filter(|path| valid_path(path))
}

fn bounded_paths(names: &BTreeMap<String, BTreeSet<String>>) -> Vec<String> {
  // Reserve space for JSON escaping and fixed fields. Overlong hints do not
  // block discovery or hide Keychain entries recovered by the helper itself.
  let mut remaining = ctl_ipc::identities::MAX_REQUEST_BYTES / 2;
  names
    .keys()
    .take_while(|path| {
      if path.len() + 8 > remaining {
        return false;
      }
      remaining -= path.len() + 8;
      true
    })
    .cloned()
    .collect()
}

fn snapshot(
  inventory: Inventory,
  names: &BTreeMap<String, BTreeSet<String>>,
  hints_complete: bool,
) -> IdentitySnapshot {
  let mut files_complete = hints_complete && inventory.file_discovery_complete;
  let mut seen = BTreeSet::new();
  let identity_files = inventory
    .identity_files
    .into_iter()
    .filter_map(|file| {
      if !valid_digest(&file.identity_id) || !seen.insert(file.identity_id.clone()) {
        files_complete = false;
        return None;
      }
      let used_by = names
        .get(&file.path)
        .map(|names| names.iter().cloned().collect())
        .unwrap_or_default();
      Some(IdentityRecord { file, used_by })
    })
    .collect();
  IdentitySnapshot {
    identity_files,
    complete: inventory.complete && files_complete,
    // Never forward free-form helper diagnostics or config contents to the UI.
    // Missing legacy metadata is explained by the import action, and Keychain
    // failures have their own categorized message rather than a file warning.
    warning: (!files_complete
      || (!inventory.complete && inventory.keychain_available && !inventory.metadata_import_required))
      .then(|| "Some identity files or host associations could not be inspected. The available files are shown.".into()),
    keychain_available: inventory.keychain_available,
    keychain_message: inventory
      .keychain_error
      .as_deref()
      .map(|code| sanitized_error(code).message),
    metadata_import_required: inventory.metadata_import_required,
    checked_at_ms: super::current_time_ms(),
  }
}

fn valid_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sanitized_error(code: &str) -> CommandErrorDto {
  let (code, message) = match code {
    "identity_invalid_request" => (
      "identity_invalid_request",
      "Select a valid key file and enter its passphrase.",
    ),
    "identity_file_changed" => (
      "identity_file_changed",
      "The key file changed. Refresh and verify the current file before saving.",
    ),
    "identity_file_missing" => (
      "identity_file_missing",
      "The key file no longer exists. Refresh the list.",
    ),
    "identity_file_unreadable" => (
      "identity_file_unreadable",
      "The key file could not be read. Check its permissions.",
    ),
    "identity_unlock_failed" => (
      "identity_unlock_failed",
      "The passphrase could not unlock this key file.",
    ),
    "identity_keychain_unavailable" => (
      "identity_keychain_unavailable",
      "Keychain is unavailable. Use a properly signed app and ctld helper.",
    ),
    "identity_keychain_locked" => (
      "identity_keychain_locked",
      "Keychain access was locked, denied, or cancelled. Unlock Keychain and try again.",
    ),
    "identity_keychain_busy" => (
      "identity_keychain_busy",
      "Another Keychain request is still active. Complete or cancel it, then try again.",
    ),
    "identity_list_failed" => (
      "identity_list_failed",
      "Saved passphrase metadata could not be read. Check Keychain access and try again.",
    ),
    "identity_unsupported" => (
      "identity_unsupported",
      "This identity-file operation is not supported on this platform or for this key format.",
    ),
    "identity_forget_failed" => (
      "identity_forget_failed",
      "The saved passphrase could not be removed. Check Keychain access and try again.",
    ),
    "identity_save_failed" => (
      "identity_save_failed",
      "The verified passphrase could not be saved. Check Keychain access and try again.",
    ),
    _ => (
      "identity_list_failed",
      "Identity files could not be inspected. Update or rebuild ctld and try again.",
    ),
  };
  CommandErrorDto::new(code, message)
}
