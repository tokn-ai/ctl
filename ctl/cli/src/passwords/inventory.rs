use ctl_ipc::{credentials, identities};
use serde::Serialize;

mod references;

#[derive(Debug, Serialize)]
pub(super) struct Snapshot {
  pub entries: Vec<Entry>,
  pub complete: bool,
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
  Unknown,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Removal<'a> {
  Credential(&'a str),
  Identity(&'a str),
}

pub(super) fn build(discovery: credentials::Discovery) -> Snapshot {
  let mut warnings = Vec::new();
  for warning in discovery.warnings {
    if !warning.trim().is_empty() && !warnings.contains(&warning) {
      warnings.push(warning);
    }
  }
  if !discovery.complete && warnings.is_empty() {
    warnings.push("Saved password discovery did not finish; some entries may be missing.".into());
  }
  let mut entries: Vec<_> = discovery
    .entries
    .into_iter()
    .map(Entry::discovered)
    .collect();
  entries.sort_by(|left, right| left.id.cmp(&right.id));
  Snapshot {
    entries,
    complete: discovery.complete,
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
  fn discovered(record: credentials::SavedPassword) -> Self {
    let (source, prefix) = match record.source {
      credentials::PasswordSource::Credential => (Source::Credential, "password"),
      credentials::PasswordSource::Identity => (Source::Identity, "identity"),
    };
    let id = format!("{prefix}:{}", record.id);
    let reference = references::key(&id, source);
    let scope_id = if source == Source::Credential {
      record.id.split_once(':').and_then(|(scope, account)| {
        [scope, account]
          .into_iter()
          .all(|component| {
            component.len() == 64 && component.bytes().all(|byte| byte.is_ascii_hexdigit())
          })
          .then(|| scope.into())
      })
    } else {
      None
    };
    Self {
      id,
      reference,
      name: record.name,
      kind: record.kind,
      source,
      state: match record.state {
        credentials::PasswordState::Saved => State::Saved,
        credentials::PasswordState::FileChanged => State::FileChanged,
        credentials::PasswordState::Unknown => State::Unknown,
      },
      scope_id,
      target: record.target,
      account: record.account,
      key_name: record.key_name,
      path: record.path,
      display_path: record.display_path,
      file_version: record.file_version,
      key_type: record.key_type,
      fingerprint: record.fingerprint,
      encrypted: record.encrypted,
      file_state: record.file_state,
      detail: record.detail,
      created_at_ms: record.created_at_ms,
      updated_at_ms: record.updated_at_ms,
      stored_id: record.id,
    }
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
      State::Unknown => "unknown",
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

#[cfg(test)]
pub(super) mod fixtures;
