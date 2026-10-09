use ctl_ipc::SshTarget;
use ctl_ipc::credentials::{Inventory, lookup_scope_ids, scope_id};
use ctl_keychain_client::{Authentication, Query, Write};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

use crate::credential_metadata::{self, Attributes, Metadata, SERVICE_PREFIX};

pub(crate) mod approval;
pub(crate) mod approval_scope;
mod availability;
mod clear;
mod discovery;
pub(crate) mod identity;
mod index;
mod operation;
mod purpose;

#[cfg(test)]
mod tests;

pub(crate) use availability::availability;

const KEYCHAIN_SERVICE_PREFIX: &str = "dev.tokn-ai.ctl.ctld.ssh";
const SAVE_POLICY_SERVICE_PREFIX: &str = "dev.tokn-ai.ctl.ctld.ssh-save-policy";
const SAVE_POLICY_ACCOUNT: &str = "policy";
const NEVER_SAVE: &[u8] = b"never";
pub const MISSING_ENTITLEMENT: i32 = -34_018;

#[derive(Debug, Clone, Copy)]
pub struct Error(pub security_framework::base::Error);

impl std::fmt::Display for Error {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(formatter)
  }
}

impl From<ctl_keychain_client::Error> for Error {
  fn from(error: ctl_keychain_client::Error) -> Self {
    Self(security_framework::base::Error::from_code(error.0))
  }
}

impl Error {
  pub fn is_missing_entitlement(self) -> bool {
    self.0.code() == MISSING_ENTITLEMENT
  }

  pub fn is_unavailable(self) -> bool {
    self.is_missing_entitlement() || self.0.code() == -25_291
  }

  pub fn is_locked(self) -> bool {
    matches!(self.0.code(), -25_308 | -25_315 | -25_293 | -128)
  }

  pub fn is_busy(self) -> bool {
    self.0.code() == operation::BUSY
  }

  pub fn is_scan_limit(self) -> bool {
    self.0.code() == ctl_keychain_client::ATTRIBUTE_SCAN_LIMIT
  }

  pub fn is_scan_conflict(self) -> bool {
    self.0.code() == ctl_keychain_client::ATTRIBUTE_SCAN_CONFLICT
  }
}

pub(crate) fn credential_name(target: &SshTarget, prompt: &str) -> String {
  purpose::credential(target, prompt)
}

pub(crate) fn load_for_connection(
  target: &SshTarget,
  prompt: &str,
  authorization: &approval::Attempt,
) -> Result<Option<Zeroizing<String>>, Error> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation = Operation::start(
    "403d4249-0cb2-4921-ba43-ae6ffb0ded6b",
    Event::CredentialRead,
    Some(&crate::target_key(target)),
  );
  let result = load_for_connection_recorded_inner(target, prompt, authorization);
  operation.finish(
    if matches!(&result, Ok(None)) {
      Outcome::Missing
    } else if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|_| "keychain_operation_failed"),
    result.as_ref().err().map(|error| error.0.code()),
  );
  result
}

fn load_for_connection_recorded_inner(
  target: &SshTarget,
  prompt: &str,
  authorization: &approval::Attempt,
) -> Result<Option<Zeroizing<String>>, Error> {
  let operation = operation::acquire()?;
  let reason = format!(
    "Read {} to authenticate this SSH connection",
    credential_name(target, prompt)
  );
  let account = digest(prompt.as_bytes());
  lookup_scopes(target, |scope| {
    let service = format!("{KEYCHAIN_SERVICE_PREFIX}.{scope}");
    authorization.read(
      &operation,
      &format!("{service}:{account}"),
      &reason,
      |authentication| {
        let records = ctl_keychain_client::search(&Query {
          service: Some(&service),
          account: Some(&account),
          limit: 1,
          secret: true,
          authentication,
        })?;
        records.into_iter().next().map(secret_string).transpose()
      },
    )
  })
}

// Only an absent exact item permits trying its legacy scope. Denied or failed
// authentication must never trigger a second lookup or authorization prompt.
fn lookup_scopes<T, E>(
  target: &SshTarget,
  mut read: impl FnMut(&str) -> Result<Option<T>, E>,
) -> Result<Option<T>, E> {
  for scope in lookup_scope_ids(target) {
    if let Some(value) = read(&scope)? {
      return Ok(Some(value));
    }
  }
  Ok(None)
}

fn replace_scopes<E>(
  target: &SshTarget,
  write: impl FnOnce(&str) -> Result<(), E>,
  mut remove_legacy: impl FnMut(&str) -> Result<(), E>,
) -> Result<(), E> {
  let scopes = lookup_scope_ids(target);
  write(&scopes[0])?;
  // Remove only after the replacement and its metadata have been saved.
  for legacy in &scopes[1..] {
    remove_legacy(legacy)?;
  }
  Ok(())
}

fn remove_scopes<E>(
  target: &SshTarget,
  mut remove: impl FnMut(&str) -> Result<(), E>,
) -> Result<(), E> {
  for scope in lookup_scope_ids(target) {
    remove(&scope)?;
  }
  Ok(())
}

fn secret_string(mut record: ctl_keychain_client::Record) -> Result<Zeroizing<String>, Error> {
  let mut bytes = record.secret.take().ok_or_else(invalid_metadata)?;
  match String::from_utf8(std::mem::take(&mut *bytes)) {
    Ok(value) => Ok(Zeroizing::new(value)),
    Err(error) => {
      let _bytes = Zeroizing::new(error.into_bytes());
      Err(invalid_metadata())
    }
  }
}

pub fn save(target: &SshTarget, secrets: &HashMap<String, Zeroizing<String>>) -> Result<(), Error> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation = Operation::start(
    "50fb5c2f-2452-4f12-a22f-2dac1e66ea82",
    Event::CredentialSave,
    Some(&crate::target_key(target)),
  );
  let result = save_recorded_inner(target, secrets);
  operation.finish(
    if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|_| "keychain_operation_failed"),
    result.as_ref().err().map(|error| error.0.code()),
  );
  result
}

fn save_recorded_inner(
  target: &SshTarget,
  secrets: &HashMap<String, Zeroizing<String>>,
) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  for (prompt, secret) in secrets {
    let metadata = Metadata::from_prompt(target, prompt);
    let comment = serde_json::to_string(&metadata).map_err(|_| invalid_metadata())?;
    let account = digest(prompt.as_bytes());
    let reason = format!(
      "Save {} in Keychain for future SSH connections",
      credential_name(target, prompt)
    );
    let pending = index::begin_secret_mutation()?;
    replace_scopes(
      target,
      |scope| {
        let service = format!("{KEYCHAIN_SERVICE_PREFIX}.{scope}");
        ctl_keychain_client::upsert(&Write {
          service: &service,
          account: &account,
          label: &metadata.name(),
          comment: &comment,
          data: secret.as_bytes(),
          user_presence: true,
          authentication: Authentication::Allow { reason: &reason },
        })?;
        let now = SystemTime::now()
          .duration_since(UNIX_EPOCH)
          .ok()
          .and_then(|value| i64::try_from(value.as_millis()).ok());
        let mut inventory = credential_metadata::inventory_from_attributes([Some(Attributes {
          values: HashMap::from([
            ("svce".into(), service),
            ("acct".into(), account.clone()),
            ("icmt".into(), comment),
          ]),
          created_at_ms: now,
          updated_at_ms: now,
        })]);
        let credential = inventory.credentials.pop().ok_or_else(invalid_metadata)?;
        index::save_credential(credential)
      },
      |scope| {
        // Replacing a credential must not leave an exact legacy copy that can
        // reappear after the newly saved item is forgotten.
        ctl_keychain_client::delete(
          &format!("{KEYCHAIN_SERVICE_PREFIX}.{scope}"),
          Some(&account),
          Authentication::Allow { reason: &reason },
        )?;
        index::forget_credential(&format!("{scope}:{account}"))
      },
    )?;
    index::finish_mutation(&pending)?;
  }
  Ok(())
}

pub fn should_offer_save(target: &SshTarget) -> Result<bool, Error> {
  availability::with_access_policy(availability, || read_save_policy(target))
}

fn read_save_policy(target: &SshTarget) -> Result<bool, Error> {
  let never = lookup_scopes(target, |scope| {
    let records = ctl_keychain_client::search(&Query {
      service: Some(&format!("{SAVE_POLICY_SERVICE_PREFIX}.{scope}")),
      account: Some(SAVE_POLICY_ACCOUNT),
      limit: 1,
      secret: true,
      authentication: Authentication::Forbid,
    })?;
    Ok::<_, Error>(
      records
        .into_iter()
        .next()
        .and_then(|record| record.secret)
        .filter(|policy| policy.as_slice() == NEVER_SAVE),
    )
  })?;
  Ok(never.is_none())
}

pub fn never_save(target: &SshTarget) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  delete_inner(target)?;
  ctl_keychain_client::upsert(&Write {
    service: &policy_service(target),
    account: SAVE_POLICY_ACCOUNT,
    label: "ctmux credential save preference",
    comment: "",
    data: NEVER_SAVE,
    user_presence: false,
    authentication: Authentication::Forbid,
  })?;
  Ok(())
}

pub fn delete(target: &SshTarget) -> Result<(), Error> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation = Operation::start(
    "a85ba442-f22f-4b58-8c01-525f82bd2e5f",
    Event::CredentialRemove,
    Some(&crate::target_key(target)),
  );
  let result = delete_recorded_inner(target);
  operation.finish(
    if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|_| "keychain_operation_failed"),
    result.as_ref().err().map(|error| error.0.code()),
  );
  result
}

fn delete_recorded_inner(target: &SshTarget) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  delete_inner(target)
}

fn delete_inner(target: &SshTarget) -> Result<(), Error> {
  let (credentials, _, _) = index::list()?;
  let pending = index::begin_secret_mutation()?;
  let reason = format!(
    "Remove saved SSH credentials for {} from Keychain",
    purpose::connection(target)
  );
  remove_scopes(target, |scope| {
    ctl_keychain_client::delete(
      &format!("{KEYCHAIN_SERVICE_PREFIX}.{scope}"),
      None,
      Authentication::Allow { reason: &reason },
    )?;
    ctl_keychain_client::delete(
      &format!("{SAVE_POLICY_SERVICE_PREFIX}.{scope}"),
      Some(SAVE_POLICY_ACCOUNT),
      Authentication::Forbid,
    )?;
    for credential in credentials
      .iter()
      .filter(|credential| credential.scope_id == scope)
    {
      index::forget_credential(&credential.credential_id)?;
    }
    Ok::<_, Error>(())
  })?;
  index::finish_mutation(&pending)
}

/// Inventory reads only the non-biometric metadata index and noninteractive
/// existence checks. It never authorizes access to a stored password.
pub fn list() -> Result<Inventory, Error> {
  availability()?;
  let (credentials, _, complete) = index::list()?;
  let metadata_import_required = index::required()?;
  Ok(Inventory {
    credentials,
    complete: complete && !metadata_import_required,
    warning: (!complete).then(|| "Some saved credential metadata could not be checked.".into()),
    metadata_import_required,
  })
}

pub fn metadata_import_required() -> Result<bool, Error> {
  index::required()
}

/// Explicit user action: import names/bindings without returning secret values.
pub fn import_metadata() -> Result<(), Error> {
  let _operation = operation::acquire()?;
  index::import()
}

/// Explicitly remove owned SSH secrets, leaving files and save preferences intact.
pub fn clear() -> Result<ctl_ipc::credentials::ClearCounts, Error> {
  clear::run()
}

/// Authoritative attribute-only discovery and best-effort metadata repair.
pub fn discover() -> Result<ctl_ipc::credentials::Discovery, Error> {
  discovery::run()
}

pub fn forget(credential_id: &str) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  let (scope_id, account_id) =
    credential_metadata::item_identity(credential_id).ok_or_else(invalid_metadata)?;
  availability()?;
  let service = format!("{SERVICE_PREFIX}{scope_id}");
  if ctl_keychain_client::exists(&service, account_id)? == ctl_keychain_client::Presence::Missing {
    return Err(invalid_metadata());
  }
  // A discovered source can exist without readable cache metadata. Its validated
  // namespace and exact ID suffice for explicit removal, never for secret reuse.
  let cached = index::list().ok().and_then(|(credentials, _, _)| {
    credentials
      .into_iter()
      .find(|credential| credential.credential_id == credential_id)
  });
  let name = cached.as_ref().map_or_else(
    || {
      format!(
        "saved SSH credential {}",
        &digest(credential_id.as_bytes())[..12]
      )
    },
    purpose::stored_credential,
  );
  let reason = format!("Remove {name} from Keychain");
  let pending = index::begin_secret_mutation()?;
  ctl_keychain_client::delete(
    &service,
    Some(account_id),
    Authentication::Allow { reason: &reason },
  )?;
  index::forget_credential(credential_id)?;
  index::finish_mutation(&pending)
}

fn invalid_metadata() -> Error {
  Error(security_framework::base::Error::from_code(-50))
}

fn policy_service(target: &SshTarget) -> String {
  format!("{SAVE_POLICY_SERVICE_PREFIX}.{}", scope_id(target))
}

fn digest(value: &[u8]) -> String {
  Sha256::digest(value)
    .iter()
    .fold(String::with_capacity(64), |mut encoded, byte| {
      write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
      encoded
    })
}
