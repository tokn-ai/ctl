//! Authoritative, attribute-only discovery; metadata sidecars are a cache.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ctl_ipc::credentials::{
  CredentialKind, Discovery, PasswordSource, PasswordState, SavedPassword,
};
use ctl_ipc::identities::FileState;
use ctl_keychain_client::{Authentication, Record};
use sha2::{Digest as _, Sha256};

use super::{Error, identity, index, invalid_metadata};
use crate::credential_metadata::{self, Attributes, SERVICE_PREFIX};
use crate::identities::{IdentityError, SavedIdentity, inspect_path};

const REASON: &str = "List saved ctmux SSH passwords and key passphrases without reading their secret values, and refresh their display metadata.";
const CACHE_WARNING: &str = "All saved items were discovered, but their display metadata cache could not be refreshed. Try listing again after checking Keychain access.";

trait Store {
  fn scan(&mut self) -> Result<Vec<Record>, Error>;
  fn reconcile(&mut self, records: &[Record]) -> Result<(), Error>;
}

struct Keychain;

impl Store for Keychain {
  fn scan(&mut self) -> Result<Vec<Record>, Error> {
    ctl_keychain_client::scan_attributes(Authentication::Allow { reason: REASON }, owned)
      .map_err(Into::into)
  }

  fn reconcile(&mut self, records: &[Record]) -> Result<(), Error> {
    index::reconcile(records)
  }
}

pub(super) fn run() -> Result<Discovery, Error> {
  let _operation = super::operation::acquire()?;
  super::availability()?;
  run_with(&mut Keychain)
}

fn owned(service: &str) -> bool {
  service.starts_with(SERVICE_PREFIX) || service == identity::SERVICE
}

fn run_with(store: &mut impl Store) -> Result<Discovery, Error> {
  let records = store.scan()?;
  let mut entries = Vec::new();
  for record in &records {
    let Some(service) = record
      .attributes
      .get("svce")
      .filter(|service| owned(service))
    else {
      continue;
    };
    if record.secret.is_some() {
      return Err(invalid_metadata());
    }
    entries.push(if service == identity::SERVICE {
      project_identity(record)
    } else {
      project_credential(record)
    });
  }
  if entries.len() > ctl_keychain_client::MAX_ATTRIBUTE_SCAN_ITEMS {
    return Err(ctl_keychain_client::Error(ctl_keychain_client::ATTRIBUTE_SCAN_LIMIT).into());
  }
  let mut identifiers = BTreeSet::new();
  if entries
    .iter()
    .any(|entry| !identifiers.insert((entry.source == PasswordSource::Identity, entry.id.as_str())))
  {
    return Err(ctl_keychain_client::Error(ctl_keychain_client::ATTRIBUTE_SCAN_CONFLICT).into());
  }
  entries.sort_by(|left, right| {
    left
      .name
      .cmp(&right.name)
      .then_with(|| left.id.cmp(&right.id))
  });
  let warnings = store
    .reconcile(&records)
    .err()
    .map(|_| vec![CACHE_WARNING.into()])
    .unwrap_or_default();
  Ok(Discovery {
    entries,
    complete: true,
    warnings,
  })
}

fn empty(record: &Record, source: PasswordSource, id: String, name: &str) -> SavedPassword {
  SavedPassword {
    id,
    source,
    name: name.into(),
    kind: match source {
      PasswordSource::Credential => CredentialKind::SshCredential,
      PasswordSource::Identity => CredentialKind::SshKeyPassphrase,
    },
    state: PasswordState::Unknown,
    target: None,
    account: None,
    key_name: None,
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
  }
}

fn project_credential(record: &Record) -> SavedPassword {
  // The legacy converter's budget applies to each item separately here, not
  // to the complete scan. Its fallback retains old hashed-only credentials.
  let credential = credential_metadata::inventory_from_attributes([Some(Attributes {
    values: record.attributes.clone(),
    created_at_ms: record.created_at_ms,
    updated_at_ms: record.updated_at_ms,
  })])
  .credentials
  .into_iter()
  .next();
  let Some(credential) = credential else {
    let mut entry = empty(
      record,
      PasswordSource::Credential,
      unknown_id(record),
      "Saved SSH credential",
    );
    entry.detail = Some("The stored identifier is invalid. Its existence is listed, but this item cannot be selected for individual removal.".into());
    return entry;
  };
  let mut entry = empty(
    record,
    PasswordSource::Credential,
    credential.credential_id,
    &credential.name,
  );
  entry.kind = credential.kind;
  entry.target = credential.target;
  entry.account = credential.account;
  entry.key_name = credential.key_name;
  if entry.target.is_some() {
    entry.state = PasswordState::Saved;
  } else {
    entry.detail = Some("The credential exists, but its descriptive metadata is missing or invalid. Its authentication usability was not checked.".into());
  }
  entry
}

fn project_identity(record: &Record) -> SavedPassword {
  let account = record.attributes.get("acct");
  let id = account
    .filter(|id| valid_identity_id(id))
    .cloned()
    .unwrap_or_else(|| unknown_id(record));
  let mut entry = empty(
    record,
    PasswordSource::Identity,
    id,
    "Saved SSH key passphrase",
  );
  let Some(metadata) = index::imported_identity(record).map(|(_, metadata)| metadata) else {
    entry.detail = Some("The passphrase exists, but its key-file binding metadata is missing or invalid. Verify and save the current key's passphrase before reuse.".into());
    return entry;
  };
  populate_identity(&mut entry, &metadata);
  entry
}

fn populate_identity(entry: &mut SavedPassword, metadata: &SavedIdentity) {
  entry.name = Path::new(&metadata.path).file_name().map_or_else(
    || "Saved SSH key passphrase".into(),
    |name| name.to_string_lossy().into_owned(),
  );
  entry.path = Some(metadata.path.clone());
  entry.display_path = Some(display_path(&metadata.path));
  match inspect_path(&metadata.path) {
    Ok(snapshot) => {
      entry.file_version = Some(snapshot.file_version.clone());
      entry.encrypted = Some(snapshot.encrypted);
      entry.file_state = Some(FileState::Ready);
      if metadata.matches(&snapshot) {
        entry.state = PasswordState::Saved;
        entry.key_type = Some(metadata.key_type.clone());
        entry.fingerprint = Some(metadata.fingerprint.clone());
      } else {
        entry.state = PasswordState::FileChanged;
        entry.detail = Some("The key file changed since this passphrase was saved. Verify and save its current passphrase before reuse.".into());
      }
    }
    Err(error) => {
      entry.state = PasswordState::FileChanged;
      entry.file_state = Some(match error {
        IdentityError::MissingFile => FileState::Missing,
        IdentityError::UnsupportedFile => FileState::Unsupported,
        _ => FileState::Unreadable,
      });
      entry.detail = Some(error.to_string());
    }
  }
}

fn valid_identity_id(id: &str) -> bool {
  id.len() == 64
    && id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unknown_id(record: &Record) -> String {
  // Malformed identifiers cannot become an ambiguous service/account selector.
  // Include sorted attributes so the fallback is stable across scan ordering.
  let attributes: BTreeMap<_, _> = record.attributes.iter().collect();
  let bytes = serde_json::to_vec(&attributes).expect("string attributes always serialize");
  format!("unknown:{:x}", Sha256::digest(bytes))
}

fn display_path(path: &str) -> String {
  dirs::home_dir()
    .and_then(|home| {
      Path::new(path)
        .strip_prefix(home)
        .ok()
        .map(Path::to_path_buf)
    })
    .map_or_else(
      || path.into(),
      |relative| format!("~/{}", relative.display()),
    )
}

#[cfg(test)]
mod tests;
