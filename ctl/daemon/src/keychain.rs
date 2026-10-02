use ctl_ipc::SshTarget;
use ctl_ipc::credentials::{Inventory, scope_id};
use ctl_keychain_client::{Authentication, Query, Write};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

use crate::credential_metadata::{self, Attributes, Metadata, SERVICE_PREFIX};

mod availability;
pub(crate) mod identity;
mod index;
mod operation;
mod purpose;

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
}

pub(crate) fn credential_name(target: &SshTarget, prompt: &str) -> String {
  purpose::credential(target, prompt)
}

pub fn load(target: &SshTarget, prompt: &str) -> Result<Option<Zeroizing<String>>, Error> {
  let _operation = operation::acquire()?;
  let reason = format!(
    "Read {} to authenticate this SSH connection",
    credential_name(target, prompt)
  );
  let records = ctl_keychain_client::search(&Query {
    service: Some(&service(target)),
    account: Some(&digest(prompt.as_bytes())),
    limit: 1,
    secret: true,
    authentication: Authentication::Allow { reason: &reason },
  })?;
  records.into_iter().next().map(secret_string).transpose()
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
  let _operation = operation::acquire()?;
  for (prompt, secret) in secrets {
    let metadata = Metadata::from_prompt(target, prompt);
    let comment = serde_json::to_string(&metadata).map_err(|_| invalid_metadata())?;
    let service = service(target);
    let account = digest(prompt.as_bytes());
    let reason = format!(
      "Save {} in Keychain for future SSH connections",
      credential_name(target, prompt)
    );
    let pending = index::begin_mutation()?;
    ctl_keychain_client::upsert(&Write {
      service: &service,
      account: &account,
      label: &metadata.name(),
      comment: &comment,
      data: secret.as_bytes(),
      biometric: true,
      authentication: Authentication::Allow { reason: &reason },
    })?;
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .ok()
      .and_then(|value| i64::try_from(value.as_millis()).ok());
    let mut inventory = credential_metadata::inventory_from_attributes([Some(Attributes {
      values: HashMap::from([
        ("svce".into(), service),
        ("acct".into(), account),
        ("icmt".into(), comment),
      ]),
      created_at_ms: now,
      updated_at_ms: now,
    })]);
    let credential = inventory.credentials.pop().ok_or_else(invalid_metadata)?;
    index::save_credential(credential)?;
    index::finish_mutation(&pending)?;
  }
  Ok(())
}

pub fn should_offer_save(target: &SshTarget) -> Result<bool, Error> {
  availability::with_access_policy(availability, || read_save_policy(target))
}

fn read_save_policy(target: &SshTarget) -> Result<bool, Error> {
  let records = ctl_keychain_client::search(&Query {
    service: Some(&policy_service(target)),
    account: Some(SAVE_POLICY_ACCOUNT),
    limit: 1,
    secret: true,
    authentication: Authentication::Forbid,
  })?;
  Ok(
    records
      .into_iter()
      .next()
      .and_then(|record| record.secret)
      .is_none_or(|policy| policy.as_slice() != NEVER_SAVE),
  )
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
    biometric: false,
    authentication: Authentication::Forbid,
  })?;
  Ok(())
}

pub fn delete(target: &SshTarget) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  delete_inner(target)
}

fn delete_inner(target: &SshTarget) -> Result<(), Error> {
  let (credentials, _, _) = index::list()?;
  let pending = index::begin_mutation()?;
  let reason = format!(
    "Remove saved SSH credentials for {} from Keychain",
    purpose::connection(target)
  );
  ctl_keychain_client::delete(
    &service(target),
    None,
    Authentication::Allow { reason: &reason },
  )?;
  let scope = scope_id(target);
  for credential in credentials
    .into_iter()
    .filter(|credential| credential.scope_id == scope)
  {
    index::forget_credential(&credential.credential_id)?;
  }
  ctl_keychain_client::delete(
    &policy_service(target),
    Some(SAVE_POLICY_ACCOUNT),
    Authentication::Forbid,
  )?;
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

pub fn forget(credential_id: &str) -> Result<(), Error> {
  let _operation = operation::acquire()?;
  let (scope_id, account_id) =
    credential_metadata::item_identity(credential_id).ok_or_else(invalid_metadata)?;
  let (credentials, _, _) = index::list()?;
  let credential = credentials
    .iter()
    .find(|credential| credential.credential_id == credential_id)
    .ok_or_else(invalid_metadata)?;
  let reason = format!(
    "Remove {} from Keychain",
    purpose::stored_credential(credential)
  );
  let pending = index::begin_mutation()?;
  ctl_keychain_client::delete(
    &format!("{SERVICE_PREFIX}{scope_id}"),
    Some(account_id),
    Authentication::Allow { reason: &reason },
  )?;
  index::forget_credential(credential_id)?;
  index::finish_mutation(&pending)
}

fn invalid_metadata() -> Error {
  Error(security_framework::base::Error::from_code(-50))
}

fn service(target: &SshTarget) -> String {
  format!("{KEYCHAIN_SERVICE_PREFIX}.{}", scope_id(target))
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
