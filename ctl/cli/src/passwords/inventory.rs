use ctl_ipc::{credentials, identities};
use serde::Serialize;

mod references;

#[derive(Debug, Serialize)]
pub(super) struct Snapshot {
  pub entries: Vec<Entry>,
  pub complete: bool,
  pub metadata_import_required: bool,
  pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Entry {
  pub id: String,
  pub name: String,
  pub kind: credentials::CredentialKind,
  source: Source,
  pub state: State,
  #[serde(skip_serializing_if = "Option::is_none")]
  scope_id: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  target: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  account: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  key_name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  display_path: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  file_version: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  key_type: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  fingerprint: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  encrypted: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  file_state: Option<identities::FileState>,
  #[serde(skip_serializing_if = "Option::is_none")]
  detail: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  created_at_ms: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  updated_at_ms: Option<i64>,
  #[serde(skip)]
  stored_id: String,
  #[serde(skip)]
  reference: String,
}

pub(super) struct Choice {
  pub id: String,
  pub name: String,
  pub hint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Source {
  Credential,
  Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
  Saved,
  FileChanged,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Removal<'a> {
  Credential(&'a str),
  Identity(&'a str),
}

pub(super) fn build(
  credentials: credentials::Inventory,
  identities: identities::Inventory,
) -> Snapshot {
  let metadata_import_required =
    credentials.metadata_import_required || identities.metadata_import_required;
  let complete = credentials.complete
    && identities.complete
    && identities.file_discovery_complete
    && identities.keychain_available
    && !metadata_import_required;
  let mut warnings = Vec::new();
  if !identities.keychain_available && identities.warning.is_none() {
    warnings
      .push("Keychain access is unavailable; saved key passphrases could not be checked.".into());
  }
  for warning in [credentials.warning, identities.warning]
    .into_iter()
    .flatten()
  {
    if !warnings.contains(&warning) {
      warnings.push(warning);
    }
  }
  if metadata_import_required {
    warnings.push(
      "Saved metadata is incomplete. Older saved credentials may not appear in this list.".into(),
    );
  } else if !complete && warnings.is_empty() {
    warnings.push("Some saved credential metadata could not be checked.".into());
  }
  let mut entries: Vec<_> = credentials
    .credentials
    .into_iter()
    .map(Entry::credential)
    .collect();
  entries.extend(
    identities
      .identity_files
      .into_iter()
      .filter_map(Entry::identity),
  );
  entries.sort_by(|left, right| left.id.cmp(&right.id));
  Snapshot {
    entries,
    complete,
    metadata_import_required,
    warnings,
  }
}

impl Snapshot {
  /// Resolve authoritative IDs, compact references, names, then ID prefixes.
  /// Refuse ambiguous selectors rather than choosing an arbitrary stored item.
  pub fn select(&self, selector: &str) -> Result<&Entry, String> {
    if selector.trim().is_empty() {
      return Err("A saved password ID or name is required.".into());
    }
    if let Some(entry) = self.entries.iter().find(|entry| entry.id == selector) {
      return Ok(entry);
    }
    if let Some(result) = references::select(&self.entries, selector) {
      return result;
    }
    let named: Vec<_> = self
      .entries
      .iter()
      .filter(|entry| entry.name == selector)
      .collect();
    if !named.is_empty() {
      return selected(&named, selector);
    }
    let prefixed: Vec<_> = self
      .entries
      .iter()
      .filter(|entry| entry.id.starts_with(selector))
      .collect();
    selected(&prefixed, selector)
  }

  pub fn render_list(&self) -> String {
    if self.entries.is_empty() {
      return if self.complete {
        "No saved passwords.".into()
      } else {
        "No saved passwords could be listed; the inventory is incomplete.".into()
      };
    }
    crate::table::format(
      ["NAME", "TYPE", "ACCOUNT", "HOST / KEY", "STATE", "ID"],
      self.entries.iter().map(|entry| {
        [
          entry.name.clone(),
          entry.kind_label().into(),
          value(entry.account.as_deref()).into(),
          entry.location().into(),
          entry.state_label().into(),
          self.short_id(entry),
        ]
      }),
    )
  }

  pub fn short_id(&self, entry: &Entry) -> String {
    references::short_id(entry, &self.entries)
  }

  pub fn render_show(&self, entry: &Entry) -> String {
    entry.render_show(&self.short_id(entry))
  }

  pub fn choices(&self) -> Vec<Choice> {
    self
      .entries
      .iter()
      .map(|entry| Choice {
        id: entry.id.clone(),
        name: entry.name.clone(),
        hint: format!(
          "{} · {} · {} · {}",
          self.short_id(entry),
          entry.kind_label(),
          value(entry.account.as_deref()),
          entry.location(),
        ),
      })
      .collect()
  }
}

fn selected<'a>(entries: &[&'a Entry], selector: &str) -> Result<&'a Entry, String> {
  match entries {
    [entry] => Ok(entry),
    [] => Err(format!(
      "No saved password matches {}.",
      crate::table::text(selector)
    )),
    _ => Err(format!(
      "Saved password selector {} is ambiguous; use a full ID from ctl passwords --json.",
      crate::table::text(selector),
    )),
  }
}

impl Entry {
  fn credential(record: credentials::StoredCredential) -> Self {
    let id = format!("password:{}", record.credential_id);
    let reference = references::key(&id, Source::Credential);
    Self {
      id,
      reference,
      name: record.name,
      kind: record.kind,
      source: Source::Credential,
      state: State::Saved,
      scope_id: Some(record.scope_id),
      target: record.target,
      account: record.account,
      key_name: record.key_name,
      path: None,
      display_path: None,
      file_version: None,
      key_type: None,
      fingerprint: None,
      encrypted: None,
      file_state: None,
      detail: None,
      created_at_ms: record.created_at_ms,
      updated_at_ms: record.updated_at_ms,
      stored_id: record.credential_id,
    }
  }

  fn identity(record: identities::IdentityFile) -> Option<Self> {
    let state = match record.passphrase_state {
      identities::PassphraseState::Saved => State::Saved,
      identities::PassphraseState::FileChanged => State::FileChanged,
      // Discovery alone cannot establish that an unknown or unsaved key has a
      // stored secret. Avoid exposing ordinary identity files as passwords.
      _ => return None,
    };
    let id = format!("identity:{}", record.identity_id);
    let reference = references::key(&id, Source::Identity);
    Some(Self {
      id,
      reference,
      name: record.display_path.clone(),
      kind: credentials::CredentialKind::SshKeyPassphrase,
      source: Source::Identity,
      state,
      scope_id: None,
      target: None,
      account: None,
      key_name: None,
      path: Some(record.path),
      display_path: Some(record.display_path),
      file_version: record.file_version,
      key_type: record.key_type,
      fingerprint: record.fingerprint,
      encrypted: record.encrypted,
      file_state: Some(record.file_state),
      detail: record.detail,
      created_at_ms: None,
      updated_at_ms: None,
      stored_id: record.identity_id,
    })
  }

  pub fn removal(&self) -> Removal<'_> {
    match self.source {
      Source::Credential => Removal::Credential(&self.stored_id),
      Source::Identity => Removal::Identity(&self.stored_id),
    }
  }

  fn render_show(&self, short_id: &str) -> String {
    let mut rows = vec![
      ["Name".into(), self.name.clone()],
      ["Type".into(), self.kind_label().into()],
      ["ID".into(), short_id.into()],
      ["State".into(), self.state_label().into()],
    ];
    for (label, value) in [
      ("Target", self.target.as_deref()),
      ("Account", self.account.as_deref()),
      ("Key name", self.key_name.as_deref()),
      (
        "Path",
        self.display_path.as_deref().or(self.path.as_deref()),
      ),
      ("Key type", self.key_type.as_deref()),
      ("Fingerprint", self.fingerprint.as_deref()),
      ("Detail", self.detail.as_deref()),
    ] {
      if let Some(value) = value {
        rows.push([label.into(), value.into()]);
      }
    }
    if let Some(value) = self.encrypted {
      rows.push(["Encrypted".into(), value.to_string()]);
    }
    if let Some(state) = self.file_state {
      rows.push(["File state".into(), file_state_label(state).into()]);
    }
    for (label, value) in [
      ("Created at (ms)", self.created_at_ms),
      ("Updated at (ms)", self.updated_at_ms),
    ] {
      if let Some(value) = value {
        rows.push([label.into(), value.to_string()]);
      }
    }
    crate::table::format(["FIELD", "VALUE"], rows)
  }

  fn location(&self) -> &str {
    value(
      self
        .display_path
        .as_deref()
        .or(self.path.as_deref())
        .or(self.key_name.as_deref())
        .or(self.target.as_deref()),
    )
  }

  fn kind_label(&self) -> &'static str {
    match (self.source, self.kind) {
      (_, credentials::CredentialKind::SshPassword) => "SSH password",
      (Source::Identity, credentials::CredentialKind::SshKeyPassphrase) => "Key passphrase",
      (Source::Credential, credentials::CredentialKind::SshKeyPassphrase) => "Legacy passphrase",
      (_, credentials::CredentialKind::SshCredential) => "SSH credential",
    }
  }

  fn state_label(&self) -> &'static str {
    match self.state {
      State::Saved => "saved",
      State::FileChanged => "file changed",
    }
  }
}

fn value(value: Option<&str>) -> &str {
  value.filter(|value| !value.is_empty()).unwrap_or("-")
}

fn file_state_label(state: identities::FileState) -> &'static str {
  match state {
    identities::FileState::Ready => "ready",
    identities::FileState::Missing => "missing",
    identities::FileState::Unreadable => "unreadable",
    identities::FileState::Unsupported => "unsupported",
  }
}

#[cfg(test)]
mod tests;
