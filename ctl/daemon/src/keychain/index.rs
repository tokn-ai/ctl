//! Noninteractive metadata sidecars. Source secrets remain in their namespaces.

use std::collections::{BTreeSet, HashMap};

use ctl_ipc::credentials::StoredCredential;
use ctl_keychain_client::{Authentication, Presence, Query, Record, Write};
use serde::{Deserialize, Serialize};

use super::Error;
use crate::credential_metadata::{self, Attributes, SERVICE_PREFIX};
use crate::identities::SavedIdentity;

pub(super) const SERVICE: &str = "dev.tokn-ai.ctl.ctld.metadata";
pub(super) const MARKER: &str = "import-complete";
const PENDING_PREFIX: &str = "pending:";
const MAX_ITEMS: usize = 8192;
const MAX_COMMENT_BYTES: usize = 32 * 1024;
type SavedIdentities = HashMap<String, SavedIdentity>;
type IndexedRecords = (Vec<StoredCredential>, SavedIdentities, bool);

struct Imported {
  credentials: Vec<StoredCredential>,
  identities: SavedIdentities,
  complete: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
  Credential {
    credential: StoredCredential,
  },
  Identity {
    identity_id: String,
    metadata: SavedIdentity,
  },
}

#[derive(Default)]
struct Index {
  credentials: Vec<StoredCredential>,
  identities: HashMap<String, SavedIdentity>,
  imported: bool,
  pending: Vec<String>,
  complete: bool,
}

impl Index {
  fn required(&self) -> bool {
    !self.imported || !self.pending.is_empty() || !self.complete
  }
}

pub(super) fn list() -> Result<IndexedRecords, Error> {
  let index = read()?;
  let mut complete = !index.required();
  let credentials = index
    .credentials
    .into_iter()
    .filter(|credential| {
      let Some((scope, account)) = credential_metadata::item_identity(&credential.credential_id)
      else {
        complete = false;
        return false;
      };
      source_present(&format!("{SERVICE_PREFIX}{scope}"), account, &mut complete)
    })
    .collect();
  let identities = index
    .identities
    .into_iter()
    .filter(|(identity_id, _)| source_present(super::identity::SERVICE, identity_id, &mut complete))
    .collect();
  Ok((credentials, identities, complete))
}

fn source_present(service: &str, account: &str, complete: &mut bool) -> bool {
  reconcile_presence(ctl_keychain_client::exists(service, account), complete)
}

fn reconcile_presence(
  presence: Result<Presence, ctl_keychain_client::Error>,
  complete: &mut bool,
) -> bool {
  match presence {
    Ok(Presence::Missing) => false,
    // Reading the WhenUnlocked index immediately precedes this exact query.
    // A protected match needs authentication to return attributes, not to show
    // the name already stored in our noninteractive index.
    Ok(Presence::Present | Presence::Protected) => true,
    Err(_) => {
      *complete = false;
      false
    }
  }
}

pub(super) fn required() -> Result<bool, Error> {
  Ok(read()?.required())
}

fn read() -> Result<Index, Error> {
  let records = ctl_keychain_client::search(&Query {
    service: Some(SERVICE),
    account: None,
    limit: MAX_ITEMS + 1,
    secret: false,
    authentication: Authentication::Forbid,
  })?;
  Ok(project(records))
}

fn project(records: Vec<Record>) -> Index {
  let mut index = Index {
    complete: records.len() <= MAX_ITEMS,
    ..Index::default()
  };
  for record in records.into_iter().take(MAX_ITEMS) {
    let Some(account) = record.attributes.get("acct") else {
      index.complete = false;
      continue;
    };
    let Some(comment) = record.attributes.get("icmt") else {
      index.complete = false;
      continue;
    };
    if record.attributes.get("svce").map(String::as_str) != Some(SERVICE) {
      index.complete = false;
      continue;
    }
    if account == MARKER {
      index.imported = comment == "1";
      continue;
    }
    if let Some(token) = account.strip_prefix(PENDING_PREFIX) {
      if uuid::Uuid::parse_str(token).is_ok() && comment == "1" {
        index.pending.push(account.clone());
      } else {
        index.complete = false;
      }
      continue;
    }
    if comment.len() > MAX_COMMENT_BYTES {
      index.complete = false;
      continue;
    }
    match serde_json::from_str::<Entry>(comment) {
      Ok(Entry::Credential { credential })
        if valid_credential(&credential)
          && *account == credential_account(&credential.credential_id) =>
      {
        index.credentials.push(credential);
      }
      Ok(Entry::Identity {
        identity_id,
        metadata,
      }) if metadata.valid(&identity_id) && *account == identity_account(&identity_id) => {
        index.identities.insert(identity_id, metadata);
      }
      _ => index.complete = false,
    }
  }
  index
}

fn valid_credential(credential: &StoredCredential) -> bool {
  credential_metadata::item_identity(&credential.credential_id)
    .is_some_and(|(scope, _)| scope == credential.scope_id)
    && valid_text(&credential.name, 512)
    && [
      &credential.target,
      &credential.account,
      &credential.key_name,
    ]
    .iter()
    .all(|value| value.as_deref().is_none_or(|value| valid_text(value, 256)))
}

fn valid_text(value: &str, limit: usize) -> bool {
  !value.is_empty() && value.chars().count() <= limit && !value.chars().any(char::is_control)
}

fn credential_account(id: &str) -> String {
  format!("credential:{id}")
}
fn identity_account(id: &str) -> String {
  format!("identity:{id}")
}

fn write(account: &str, comment: &str) -> Result<(), Error> {
  if comment.len() > MAX_COMMENT_BYTES {
    return Err(invalid());
  }
  ctl_keychain_client::upsert(&Write {
    service: SERVICE,
    account,
    label: "ctmux credential metadata",
    comment,
    data: b"",
    biometric: false,
    authentication: Authentication::Forbid,
  })
  .map_err(Into::into)
}

pub(super) fn save_credential(mut credential: StoredCredential) -> Result<(), Error> {
  if !valid_credential(&credential) {
    return Err(invalid());
  }
  let account = credential_account(&credential.credential_id);
  // Refreshing metadata must preserve when the credential was first saved.
  let records = ctl_keychain_client::search(&Query {
    service: Some(SERVICE),
    account: Some(&account),
    limit: 1,
    secret: false,
    authentication: Authentication::Forbid,
  })?;
  if let Some(previous) = project(records)
    .credentials
    .into_iter()
    .find(|previous| previous.credential_id == credential.credential_id)
  {
    credential.created_at_ms = previous.created_at_ms.or(credential.created_at_ms);
  }
  let comment = serde_json::to_string(&Entry::Credential { credential }).map_err(|_| invalid())?;
  write(&account, &comment)
}

pub(super) fn save_identity(identity_id: &str, metadata: &SavedIdentity) -> Result<(), Error> {
  if !metadata.valid(identity_id) {
    return Err(invalid());
  }
  let comment = serde_json::to_string(&Entry::Identity {
    identity_id: identity_id.into(),
    metadata: metadata.clone(),
  })
  .map_err(|_| invalid())?;
  write(&identity_account(identity_id), &comment)
}

pub(super) fn identity_metadata(identity_id: &str) -> Result<Option<SavedIdentity>, Error> {
  if !valid_identity_id(identity_id) {
    return Err(invalid());
  }
  let account = identity_account(identity_id);
  let records = ctl_keychain_client::search(&Query {
    service: Some(SERVICE),
    account: Some(&account),
    limit: 1,
    secret: false,
    authentication: Authentication::Forbid,
  })?;
  Ok(project(records).identities.remove(identity_id))
}

pub(super) fn forget_credential(credential_id: &str) -> Result<(), Error> {
  if credential_metadata::item_identity(credential_id).is_none() {
    return Err(invalid());
  }
  remove(&credential_account(credential_id))
}

pub(super) fn forget_identity(identity_id: &str) -> Result<(), Error> {
  if !valid_identity_id(identity_id) {
    return Err(invalid());
  }
  remove(&identity_account(identity_id))
}

fn valid_identity_id(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn remove(account: &str) -> Result<(), Error> {
  ctl_keychain_client::delete(SERVICE, Some(account), Authentication::Forbid).map_err(Into::into)
}

/// Persist before changing a secret. Only successful index commit clears it.
pub(super) fn begin_mutation() -> Result<String, Error> {
  let token = uuid::Uuid::new_v4().to_string();
  write(&format!("{PENDING_PREFIX}{token}"), "1")?;
  Ok(token)
}

pub(super) fn finish_mutation(token: &str) -> Result<(), Error> {
  if uuid::Uuid::parse_str(token).is_err() {
    return Err(invalid());
  }
  remove(&format!("{PENDING_PREFIX}{token}"))
}

/// Called only after a complete clear plan has removed every owned secret.
/// A failed reset leaves import required rather than reporting an empty index.
pub(super) fn reset_empty() -> Result<(), Error> {
  ctl_keychain_client::delete(SERVICE, None, Authentication::Forbid)?;
  write(MARKER, "1")
}

pub(super) fn import() -> Result<(), Error> {
  // The caller holds the cross-process operation lock throughout import.
  // A failed/cancelled scan leaves the old index and its pending marker intact.
  let _pending = begin_mutation()?;
  let records = ctl_keychain_client::scan_attributes(
    Authentication::Allow {
      reason: "Import names and metadata for saved ctmux SSH passwords and identity passphrases, and reconcile interrupted credential updates without returning password or passphrase values.",
    },
    |service| service.starts_with(SERVICE_PREFIX) || service == super::identity::SERVICE,
  )?;
  let mut imported = imported_records(records);
  preserve_cached_hints(&mut imported);
  if !imported.complete {
    // Useful rows remain recoverable when an unrelated owned item is malformed
    // or the bounded scan is truncated. Retain the old index and pending marker:
    // neither partial recovery nor a write failure can claim a complete import.
    for credential in imported.credentials {
      save_credential(credential)?;
    }
    for (identity_id, metadata) in imported.identities {
      save_identity(&identity_id, &metadata)?;
    }
    return Err(invalid());
  }
  // Rebuild only the metadata service, so malformed and stale sidecars can be
  // recovered too. Before the final marker, a crash always requires import.
  replace(imported)
}

/// Refresh only readable metadata after a successful authoritative source scan.
/// Unknown source entries leave the cache incomplete without hiding them from
/// discovery, whose coverage is independent of whether metadata can be decoded.
pub(super) fn reconcile(records: &[Record]) -> Result<(), Error> {
  let mut imported = imported_sources(records);
  preserve_cached_hints(&mut imported);
  replace(imported)
}

fn preserve_cached_hints(imported: &mut Imported) {
  if let Ok(previous) = read() {
    preserve_public_hints(&mut imported.identities, &previous.identities);
  }
}

fn replace(imported: Imported) -> Result<(), Error> {
  // Reserve one cache row for its completion or pending marker. A source scan
  // can still be complete when this optional cache exceeds its own budget.
  if imported.credentials.len() + imported.identities.len() >= MAX_ITEMS {
    return Err(invalid());
  }
  ctl_keychain_client::delete(SERVICE, None, Authentication::Forbid)?;
  let token = begin_mutation()?;
  for credential in imported.credentials {
    save_credential(credential)?;
  }
  for (identity_id, metadata) in imported.identities {
    save_identity(&identity_id, &metadata)?;
  }
  if imported.complete {
    write(MARKER, "1")?;
    finish_mutation(&token)?;
  }
  Ok(())
}

fn preserve_public_hints(imported: &mut SavedIdentities, previous: &SavedIdentities) {
  for (identity_id, metadata) in imported {
    if metadata.public_key.is_none()
      && let Some(prior) = previous.get(identity_id)
      && prior.valid(identity_id)
      && metadata.path == prior.path
      && metadata.file_version == prior.file_version
      && metadata.key_type == prior.key_type
      && metadata.fingerprint == prior.fingerprint
    {
      metadata.public_key.clone_from(&prior.public_key);
    }
  }
}

fn imported_records(records: Vec<Record>) -> Imported {
  let mut imported = imported_sources(&records);
  let sidecars = records
    .into_iter()
    .filter(|record| record.attributes.get("svce").map(String::as_str) == Some(SERVICE))
    .collect();
  preserve_public_hints(&mut imported.identities, &project(sidecars).identities);
  imported
}

fn imported_sources(records: &[Record]) -> Imported {
  let mut credentials = Vec::new();
  let mut identities = HashMap::new();
  let mut seen = BTreeSet::new();
  let mut complete = true;
  let mut owned_count = 0;
  for record in records {
    if record.attributes.get("svce").is_some_and(|service| {
      service.starts_with(SERVICE_PREFIX) || service == super::identity::SERVICE
    }) {
      owned_count += 1;
      complete &= owned_count <= MAX_ITEMS;
    }
    match record.attributes.get("svce").map(String::as_str) {
      Some(service) if service.starts_with(SERVICE_PREFIX) => {
        let inventory = credential_metadata::inventory_from_attributes([Some(Attributes {
          values: record.attributes.clone(),
          created_at_ms: record.created_at_ms,
          updated_at_ms: record.updated_at_ms,
        })]);
        complete &= inventory.complete;
        for credential in inventory.credentials {
          // A generic fallback fully represents an old hashed-only source
          // item, even though its descriptive metadata remains unknown.
          if seen.insert(credential.credential_id.clone()) {
            credentials.push(credential);
          } else {
            complete = false;
          }
        }
      }
      Some(super::identity::SERVICE) => {
        if let Some((account, metadata)) = imported_identity(record) {
          if identities.insert(account, metadata).is_some() {
            complete = false;
          }
        } else {
          complete = false;
        }
      }
      None => complete = false,
      _ => {}
    }
    if record.secret.is_some() {
      complete = false;
    }
  }
  Imported {
    credentials,
    identities,
    complete,
  }
}

pub(super) fn imported_identity(record: &Record) -> Option<(String, SavedIdentity)> {
  let account = record.attributes.get("acct")?;
  let comment = record
    .attributes
    .get("icmt")
    .filter(|value| value.len() <= MAX_COMMENT_BYTES)?;
  let metadata: SavedIdentity = serde_json::from_str(comment).ok()?;
  metadata.valid(account).then(|| (account.clone(), metadata))
}

fn invalid() -> Error {
  Error(security_framework::base::Error::from_code(-50))
}

#[cfg(test)]
mod tests;
