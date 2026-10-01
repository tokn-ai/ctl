//! Metadata inventory across saved SSH and VPN authentication sources.
// Tauri consumes request values from IPC.
#![allow(clippy::needless_pass_by_value)]

#[cfg(any(target_os = "macos", all(test, unix)))]
mod helper;
pub(super) mod identities;
mod models;
#[cfg(unix)]
mod process;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use ctl_ipc::credentials::{Inventory, StoredCredential, scope_id};
#[cfg(target_os = "macos")]
use ctl_ipc::credentials::{Request, Response};

use crate::error::{CommandErrorDto, CommandResult};
use crate::vpn::{CredentialMetadata, CredentialSettings};
use models::{
  CredentialAction, CredentialKind, CredentialRecord, CredentialSource, CredentialStorage,
  NamedTarget, SourceState, SourceStatus,
};
pub use models::{CredentialsSnapshot, ForgetRequest, ListRequest};

const MAX_TARGETS: usize = 512;

struct HostMetadata {
  names: BTreeSet<String>,
  target: String,
  account: Option<String>,
}

#[tauri::command]
pub async fn list_saved_credentials(
  app: tauri::AppHandle,
  request: ListRequest,
) -> CommandResult<CredentialsSnapshot> {
  let (hosts, host_names_complete) = host_metadata(&request.targets);
  let (keychain, vpn) = tokio::join!(read_keychain(), crate::vpn::load_vpn_connections(app));
  let vpn = vpn
    .map(|snapshot| crate::vpn::credential_metadata(&snapshot))
    .map_err(|_| ());
  let mut result = snapshot(&hosts, keychain, vpn, current_time_ms());
  if !host_names_complete {
    add_host_name_warning(&mut result);
  }
  Ok(result)
}

#[tauri::command]
pub async fn forget_saved_credential(request: ForgetRequest) -> CommandResult<()> {
  let credential_id = backend_id(&request.credential_id)?;
  #[cfg(target_os = "macos")]
  {
    match helper::request(Request::Forget { credential_id }).await? {
      Response::Forgotten => Ok(()),
      _ => Err(unexpected_response()),
    }
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = credential_id;
    Err(unsupported_keychain())
  }
}

/// User-initiated import; ordinary inventory refreshes never authenticate.
#[tauri::command]
pub async fn import_credential_metadata() -> CommandResult<()> {
  #[cfg(target_os = "macos")]
  {
    match helper::request(Request::ImportMetadata).await? {
      Response::Imported => Ok(()),
      _ => Err(unexpected_response()),
    }
  }
  #[cfg(not(target_os = "macos"))]
  Err(unsupported_keychain())
}

#[cfg(target_os = "macos")]
async fn read_keychain() -> CommandResult<Inventory> {
  match helper::request(Request::ListMetadata).await? {
    Response::Inventory { inventory } => Ok(inventory),
    _ => Err(unexpected_response()),
  }
}

#[cfg(not(target_os = "macos"))]
fn read_keychain() -> std::future::Ready<CommandResult<Inventory>> {
  std::future::ready(Err(unsupported_keychain()))
}

#[cfg(target_os = "macos")]
fn unexpected_response() -> CommandErrorDto {
  CommandErrorDto::new(
    "credential_helper_invalid_response",
    "The credential helper returned an unexpected response. Update or rebuild ctld and try again.",
  )
}

#[cfg(not(target_os = "macos"))]
fn unsupported_keychain() -> CommandErrorDto {
  CommandErrorDto::new(
    "credentials_unsupported",
    "Saved SSH credentials require macOS Keychain.",
  )
}

fn valid_backend_id(value: &str) -> bool {
  value.len() == 129
    && value.as_bytes()[64] == b':'
    && value
      .bytes()
      .enumerate()
      .all(|(index, byte)| index == 64 || byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn backend_id(value: &str) -> CommandResult<String> {
  value
    .strip_prefix("keychain:")
    .filter(|value| valid_backend_id(value))
    .map(str::to_owned)
    .ok_or_else(|| {
      CommandErrorDto::new(
        "invalid_credential_id",
        "Select a saved Keychain credential to forget.",
      )
    })
}

fn host_metadata(targets: &[NamedTarget]) -> (BTreeMap<String, HostMetadata>, bool) {
  let mut complete = targets.len() <= MAX_TARGETS;
  let mut hosts = BTreeMap::<String, HostMetadata>::new();
  for named in targets.iter().take(MAX_TARGETS) {
    if !valid_text(&named.name, 256, true) {
      complete = false;
      continue;
    }
    let Ok(target) = named.target.to_ssh_target() else {
      complete = false;
      continue;
    };
    if !valid_target(&target) {
      complete = false;
      continue;
    }
    let metadata = hosts
      .entry(scope_id(&target))
      .or_insert_with(|| HostMetadata {
        names: BTreeSet::new(),
        target: target
          .hostname
          .clone()
          .unwrap_or_else(|| target.destination.clone()),
        account: target.user.clone(),
      });
    metadata.names.insert(named.name.clone());
  }
  (hosts, complete)
}

fn valid_target(target: &ctl_ipc::SshTarget) -> bool {
  let optional = |value: &Option<String>| {
    value
      .as_ref()
      .is_none_or(|value| valid_text(value, 1024, false))
  };
  let path = |value: &Option<std::path::PathBuf>| {
    value
      .as_ref()
      .is_none_or(|value| valid_text(&value.to_string_lossy(), 4096, true))
  };
  valid_text(&target.destination, 1024, false)
    && !target.destination.starts_with('-')
    && optional(&target.hostname)
    && optional(&target.user)
    && path(&target.identity_file)
    && target.port != Some(0)
    && target.ssh_config_alias.as_ref().is_none_or(|alias| {
      alias == &target.destination && !alias.starts_with('!') && !alias.contains(['*', '?'])
    })
    && target.gateways.len() <= 8
    && target.gateways.iter().enumerate().all(|(index, gateway)| {
      gateway.has_valid_vpn_configuration()
        && valid_text(&gateway.destination, 1024, false)
        && optional(&gateway.hostname)
        && optional(&gateway.user)
        && path(&gateway.identity_file)
        && gateway.port != Some(0)
        && (index == 0 || gateway.kind != ctl_ipc::GatewayKind::Vpn)
    })
    && serde_json::to_vec(target).is_ok_and(|bytes| bytes.len() <= 32 * 1024)
}

fn valid_text(value: &str, maximum: usize, whitespace: bool) -> bool {
  !value.trim().is_empty()
    && value.len() <= maximum
    && !value
      .chars()
      .any(|ch| ch.is_control() || (!whitespace && ch.is_whitespace()))
}

fn add_host_name_warning(snapshot: &mut CredentialsSnapshot) {
  let Some(source) = snapshot
    .sources
    .iter_mut()
    .find(|source| source.source == CredentialSource::Keychain)
  else {
    return;
  };
  if matches!(source.state, SourceState::Ready | SourceState::Partial) {
    source.state = SourceState::Partial;
    let warning = "Some saved host names could not be matched; saved credentials are still listed.";
    source.message = Some(
      source
        .message
        .take()
        .map_or_else(|| warning.into(), |message| format!("{message} {warning}")),
    );
  }
}

fn snapshot(
  hosts: &BTreeMap<String, HostMetadata>,
  keychain: CommandResult<Inventory>,
  vpn: Result<Vec<CredentialMetadata>, ()>,
  checked_at_ms: i64,
) -> CredentialsSnapshot {
  let mut credentials = Vec::new();
  let metadata_import_required = keychain
    .as_ref()
    .is_ok_and(|inventory| inventory.metadata_import_required);
  let keychain_source = match keychain {
    Ok(inventory) => {
      let mut complete = inventory.complete;
      let mut seen = BTreeSet::new();
      for credential in inventory.credentials {
        if !valid_backend_id(&credential.credential_id)
          || !seen.insert(credential.credential_id.clone())
        {
          complete = false;
          continue;
        }
        credentials.push(keychain_record(credential, hosts));
      }
      SourceStatus {
        source: CredentialSource::Keychain,
        state: if complete {
          SourceState::Ready
        } else {
          SourceState::Partial
        },
        message: (!complete)
          .then(|| "Some saved SSH credential metadata could not be read.".into()),
      }
    }
    Err(error) => SourceStatus {
      source: CredentialSource::Keychain,
      state: if error.code == "credentials_unsupported" {
        SourceState::Unsupported
      } else {
        SourceState::Unavailable
      },
      message: Some(error.message),
    },
  };
  let vpn_source = match vpn {
    Ok(connections) => {
      credentials.extend(connections.into_iter().filter_map(vpn_record));
      SourceStatus {
        source: CredentialSource::Vpn,
        state: SourceState::Ready,
        message: None,
      }
    }
    Err(()) => SourceStatus {
      source: CredentialSource::Vpn,
      state: SourceState::Unavailable,
      message: Some(
        "Saved VPN connection metadata could not be read. Check the VPN page and try again.".into(),
      ),
    },
  };
  credentials.sort_by(|left, right| {
    left
      .name
      .to_lowercase()
      .cmp(&right.name.to_lowercase())
      .then_with(|| left.credential_id.cmp(&right.credential_id))
  });
  CredentialsSnapshot {
    credentials,
    sources: vec![keychain_source, vpn_source],
    metadata_import_required,
    checked_at_ms,
  }
}

fn keychain_record(
  credential: StoredCredential,
  hosts: &BTreeMap<String, HostMetadata>,
) -> CredentialRecord {
  let host = hosts.get(&credential.scope_id);
  let kind = match credential.kind {
    ctl_ipc::credentials::CredentialKind::SshPassword => CredentialKind::SshPassword,
    ctl_ipc::credentials::CredentialKind::SshKeyPassphrase => CredentialKind::SshKeyPassphrase,
    ctl_ipc::credentials::CredentialKind::SshCredential => CredentialKind::SshCredential,
  };
  let key_name = clean(credential.key_name).and_then(|value| {
    value
      .rsplit(['/', '\\'])
      .find(|part| !part.is_empty())
      .map(str::to_owned)
  });
  let name = host.map_or_else(
    || {
      clean(Some(credential.name))
        .unwrap_or_else(|| format!("Saved SSH credential {}", &credential.credential_id[65..73]))
    },
    |host| {
      let mut name = host
        .names
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
      if host.names.len() > 3 {
        write!(name, " (+{})", host.names.len() - 3).expect("writing to a String cannot fail");
      }
      if let Some(key) = &key_name {
        write!(name, " · {key}").expect("writing to a String cannot fail");
      }
      name
    },
  );
  let detail = if host.is_none() {
    Some("No saved host currently matches this credential.".into())
  } else {
    key_name.map(|key| format!("SSH key: {key}"))
  };
  CredentialRecord {
    credential_id: format!("keychain:{}", credential.credential_id),
    name,
    kind,
    storage: CredentialStorage::Keychain,
    target: clean(credential.target).or_else(|| host.map(|host| host.target.clone())),
    account: clean(credential.account).or_else(|| host.and_then(|host| host.account.clone())),
    created_at_ms: credential.created_at_ms,
    updated_at_ms: credential.updated_at_ms,
    detail,
    action: CredentialAction::Forget,
    vpn_connection_id: None,
  }
}

fn vpn_record(connection: CredentialMetadata) -> Option<CredentialRecord> {
  let (kind, storage, target, account, detail) = match connection.settings {
    CredentialSettings::Openconnect {
      url,
      username,
      has_password: true,
    } => (
      CredentialKind::VpnPassword,
      CredentialStorage::VpnSettings,
      url_origin(&url),
      clean(Some(username)),
      "Password saved in VPN settings. Manage it on the VPN page.",
    ),
    CredentialSettings::Openconnect {
      has_password: false,
      ..
    } => return None,
    CredentialSettings::Tailscale { hostname } => (
      CredentialKind::TailscaleSignIn,
      CredentialStorage::ContainerVolume,
      clean(hostname),
      None,
      "Saved Tailscale connection. Sign-in state is unverified; any persisted sign-in is held in its container volume.",
    ),
  };
  Some(CredentialRecord {
    credential_id: format!("vpn:{}", connection.connection_id),
    name: clean(Some(connection.name)).unwrap_or_else(|| "Saved VPN connection".into()),
    kind,
    storage,
    target,
    account,
    created_at_ms: None,
    updated_at_ms: None,
    detail: Some(detail.into()),
    action: CredentialAction::ManageVpn,
    vpn_connection_id: Some(connection.connection_id),
  })
}

fn url_origin(value: &str) -> Option<String> {
  let value = value.trim();
  let normalized = if value.contains("://") {
    value.to_owned()
  } else {
    format!("https://{value}")
  };
  let url = tauri::Url::parse(&normalized).ok()?;
  if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
    return None;
  }
  Some(url.origin().ascii_serialization())
}

fn clean(value: Option<String>) -> Option<String> {
  value
    .map(|value| {
      value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(1024)
        .collect::<String>()
    })
    .filter(|value| !value.trim().is_empty())
}

fn current_time_ms() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .ok()
    .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
    .unwrap_or_default()
}
