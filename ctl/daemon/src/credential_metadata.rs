//! Pure metadata conversion, kept separate from Keychain secret operations.

#[cfg(any(target_os = "macos", test))]
use {
  ctld_ipc::SshTarget,
  ctld_ipc::credentials::{CredentialKind, Inventory, StoredCredential},
  serde::{Deserialize, Serialize},
  std::collections::HashMap,
};

#[cfg(any(target_os = "macos", test))]
pub(crate) const SERVICE_PREFIX: &str = "io.rmux.desktop.ctld.ssh.";
#[cfg(any(target_os = "macos", test))]
pub(crate) const MAX_SEARCH_ITEMS: usize = 4096;
#[cfg(any(target_os = "macos", test))]
const MAX_TEXT_CHARACTERS: usize = 256;
#[cfg(any(target_os = "macos", test))]
const MAX_METADATA_BYTES: usize = 8192;
#[cfg(any(target_os = "macos", test))]
const METADATA_VERSION: u8 = 1;
#[cfg(any(target_os = "macos", test))]
const INCOMPLETE_WARNING: &str =
  "Some saved credentials could not be listed. Refresh after checking Keychain access.";
pub(crate) const TRUNCATED_WARNING: &str =
  "There are more saved credentials than this page can display. The list is incomplete.";

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Metadata {
  version: u8,
  kind: CredentialKind,
  target: String,
  account: Option<String>,
  key_name: Option<String>,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
pub(crate) struct Attributes {
  pub values: HashMap<String, String>,
  pub created_at_ms: Option<i64>,
  pub updated_at_ms: Option<i64>,
}

#[cfg(any(target_os = "macos", test))]
impl Metadata {
  pub(crate) fn from_prompt(target: &SshTarget, prompt: &str) -> Self {
    let kind = if prompt
      .to_lowercase()
      .starts_with("enter passphrase for key")
    {
      CredentialKind::SshKeyPassphrase
    } else if prompt.to_lowercase().contains("password:") {
      CredentialKind::SshPassword
    } else {
      CredentialKind::SshCredential
    };
    // Do not persist the prompt: keyboard-interactive text can be arbitrary.
    // Only a standard OpenSSH key prompt contributes a file basename.
    let key_name = if kind == CredentialKind::SshKeyPassphrase {
      prompt
        .strip_prefix("Enter passphrase for key '")
        .and_then(|value| value.trim_end().strip_suffix("':"))
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .and_then(clean_text)
    } else {
      None
    };
    Self {
      version: METADATA_VERSION,
      kind,
      target: clean_text(&target.destination).unwrap_or_default(),
      account: target
        .user
        .as_deref()
        .or_else(|| target.destination.rsplit_once('@').map(|(user, _)| user))
        .and_then(clean_text),
      key_name,
    }
  }

  pub(crate) fn name(&self) -> String {
    let label = match self.kind {
      CredentialKind::SshPassword => "SSH password",
      CredentialKind::SshKeyPassphrase => "SSH key passphrase",
      CredentialKind::SshCredential => "SSH credential",
    };
    let detail = self.key_name.as_deref().unwrap_or(&self.target);
    format!("{label} · {detail}")
  }

  fn from_json(value: &str) -> Option<Self> {
    if value.len() > MAX_METADATA_BYTES {
      return None;
    }
    let mut metadata: Self = serde_json::from_str(value).ok()?;
    if metadata.version != METADATA_VERSION {
      return None;
    }
    metadata.target = clean_text(&metadata.target)?;
    metadata.account = metadata.account.as_deref().and_then(clean_text);
    metadata.key_name = metadata.key_name.as_deref().and_then(clean_text);
    Some(metadata)
  }
}

pub(crate) fn item_identity(credential_id: &str) -> Option<(&str, &str)> {
  let (scope_id, account_id) = credential_id.split_once(':')?;
  (valid_digest(scope_id) && valid_digest(account_id)).then_some((scope_id, account_id))
}

fn valid_digest(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(any(target_os = "macos", test))]
fn clean_text(value: &str) -> Option<String> {
  let value: String = value
    .chars()
    .filter(|character| !character.is_control())
    .take(MAX_TEXT_CHARACTERS)
    .collect();
  let value = value.trim();
  (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn inventory_from_attributes(
  attributes: impl IntoIterator<Item = Option<Attributes>>,
) -> Inventory {
  let mut inventory = Inventory {
    credentials: Vec::new(),
    complete: true,
    warning: None,
  };
  let mut count = 0;
  for attributes in attributes {
    count += 1;
    if count > MAX_SEARCH_ITEMS {
      inventory.complete = false;
      inventory.warning = Some(TRUNCATED_WARNING.into());
      break;
    }
    let Some(Attributes {
      values: attributes,
      created_at_ms,
      updated_at_ms,
    }) = attributes
    else {
      inventory.complete = false;
      continue;
    };
    let Some(service) = attributes.get("svce") else {
      inventory.complete = false;
      continue;
    };
    let Some(scope_id) = service.strip_prefix(SERVICE_PREFIX) else {
      continue;
    };
    let Some(account_id) = attributes.get("acct") else {
      inventory.complete = false;
      continue;
    };
    if !valid_digest(scope_id) || !valid_digest(account_id) {
      inventory.complete = false;
      continue;
    }
    let metadata = attributes
      .get("icmt")
      .and_then(|value| Metadata::from_json(value));
    let mut credential = StoredCredential {
      credential_id: format!("{scope_id}:{account_id}"),
      scope_id: scope_id.into(),
      name: format!("Saved SSH credential · {}", &account_id[..8]),
      kind: CredentialKind::SshCredential,
      target: None,
      account: None,
      key_name: None,
      created_at_ms,
      updated_at_ms,
    };
    if let Some(metadata) = metadata {
      credential.name = metadata.name();
      credential.kind = metadata.kind;
      credential.target = Some(metadata.target);
      credential.account = metadata.account;
      credential.key_name = metadata.key_name;
    }
    inventory.credentials.push(credential);
  }
  inventory.credentials.sort_by(|left, right| {
    left
      .name
      .cmp(&right.name)
      .then_with(|| left.credential_id.cmp(&right.credential_id))
  });
  if !inventory.complete && inventory.warning.is_none() {
    inventory.warning = Some(INCOMPLETE_WARNING.into());
  }
  inventory
}

#[cfg(test)]
mod tests;
