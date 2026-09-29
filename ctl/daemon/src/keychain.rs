use ctld_ipc::SshTarget;
use ctld_ipc::credentials::{Inventory, scope_id};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{
  AccessControlOptions, PasswordOptions, delete_generic_password_options, generic_password,
  set_generic_password_options,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use zeroize::Zeroizing;

use crate::credential_metadata::{self, MAX_SEARCH_ITEMS, Metadata, SERVICE_PREFIX};

mod attributes;

const KEYCHAIN_SERVICE_PREFIX: &str = "io.rmux.desktop.ctld.ssh";
const SAVE_POLICY_SERVICE_PREFIX: &str = "io.rmux.desktop.ctld.ssh-save-policy";
const SAVE_POLICY_ACCOUNT: &str = "policy";
const NEVER_SAVE: &[u8] = b"never";
const ITEM_NOT_FOUND: i32 = -25_300;
pub const MISSING_ENTITLEMENT: i32 = -34_018;

pub struct Error(pub security_framework::base::Error);

impl std::fmt::Display for Error {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(formatter)
  }
}

impl Error {
  pub fn is_missing_entitlement(&self) -> bool {
    self.0.code() == MISSING_ENTITLEMENT
  }
}

pub fn load(target: &SshTarget, prompt: &str) -> Result<Option<Zeroizing<String>>, Error> {
  let mut options = password_options(target, prompt);
  options.use_protected_keychain();
  let bytes = match generic_password(options) {
    Ok(bytes) => Zeroizing::new(bytes),
    Err(error) if error.code() == ITEM_NOT_FOUND => return Ok(None),
    Err(error) => return Err(Error(error)),
  };
  let mut bytes = bytes;
  String::from_utf8(std::mem::take(&mut *bytes))
    .map(Zeroizing::new)
    .map(Some)
    .map_err(|error| {
      let mut bytes = error.into_bytes();
      bytes.fill(0);
      Error(security_framework::base::Error::from_code(-26_275))
    })
}

pub fn save(target: &SshTarget, secrets: &HashMap<String, Zeroizing<String>>) -> Result<(), Error> {
  for (prompt, secret) in secrets {
    let mut options = password_options(target, prompt);
    options.use_protected_keychain();
    match delete_generic_password_options(options) {
      Ok(()) => {}
      Err(error) if error.code() == ITEM_NOT_FOUND => {}
      Err(error) => return Err(Error(error)),
    }
    let mut options = password_options(target, prompt);
    options.use_protected_keychain();
    let access_control = SecAccessControl::create_with_protection(
      Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
      AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
    )
    .map_err(Error)?;
    options.set_access_control(access_control);
    let metadata = Metadata::from_prompt(target, prompt);
    options.set_label(&metadata.name());
    options.set_description("rmux saved SSH credential");
    options
      .set_comment(&serde_json::to_string(&metadata).expect("credential metadata is serializable"));
    set_generic_password_options(secret.as_bytes(), options).map_err(Error)?;
  }
  Ok(())
}

pub fn should_offer_save(target: &SshTarget) -> Result<bool, Error> {
  let mut options = save_policy_options(target);
  options.use_protected_keychain();
  match generic_password(options) {
    Ok(policy) => Ok(policy != NEVER_SAVE),
    Err(error) if error.code() == ITEM_NOT_FOUND => Ok(true),
    Err(error) => Err(Error(error)),
  }
}

pub fn never_save(target: &SshTarget) -> Result<(), Error> {
  delete(target)?;
  let mut options = save_policy_options(target);
  options.use_protected_keychain();
  let access_control = SecAccessControl::create_with_protection(
    Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
    AccessControlOptions::empty().bits(),
  )
  .map_err(Error)?;
  options.set_access_control(access_control);
  set_generic_password_options(NEVER_SAVE, options).map_err(Error)
}

pub fn delete(target: &SshTarget) -> Result<(), Error> {
  let mut credentials = ItemSearchOptions::new();
  credentials
    .class(ItemClass::generic_password())
    .service(&service(target))
    .ignore_legacy_keychains();
  match credentials.delete() {
    Ok(()) => {}
    Err(error) if error.code() == ITEM_NOT_FOUND => {}
    Err(error) => return Err(Error(error)),
  }
  let mut policy = save_policy_options(target);
  policy.use_protected_keychain();
  match delete_generic_password_options(policy) {
    Ok(()) => Ok(()),
    Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
    Err(error) => Err(Error(error)),
  }
}

/// Return attributes from the protected Keychain without requesting secret data.
pub fn list() -> Result<Inventory, Error> {
  let mut options = ItemSearchOptions::new();
  options
    .class(ItemClass::generic_password())
    .ignore_legacy_keychains()
    .load_attributes(true)
    .load_data(false)
    .load_refs(false)
    .limit(i64::try_from(MAX_SEARCH_ITEMS + 1).expect("bounded Keychain search limit"));
  // Do not use authenticationUISkip: it can silently omit protected entries.
  // Attribute-only retrieval does not ask for the biometric-protected secret.
  // If macOS denies access, report an error rather than presenting an empty list.
  let results = match options.search() {
    Ok(results) => results,
    Err(error) if error.code() == ITEM_NOT_FOUND => Vec::new(),
    Err(error) => return Err(Error(error)),
  };
  Ok(credential_metadata::inventory_from_attributes(
    results.iter().map(attributes::read),
  ))
}

/// Delete exactly one owned item. Host save preferences and live masters remain.
pub fn forget(credential_id: &str) -> Result<(), Error> {
  let (scope_id, account_id) = credential_metadata::item_identity(credential_id)
    .ok_or_else(|| Error(security_framework::base::Error::from_code(-50)))?;
  let mut options = ItemSearchOptions::new();
  options
    .class(ItemClass::generic_password())
    .service(&format!("{SERVICE_PREFIX}{scope_id}"))
    .account(account_id)
    .ignore_legacy_keychains();
  match options.delete() {
    Ok(()) => Ok(()),
    Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
    Err(error) => Err(Error(error)),
  }
}

fn password_options(target: &SshTarget, prompt: &str) -> PasswordOptions {
  PasswordOptions::new_generic_password(&service(target), &digest(prompt.as_bytes()))
}

fn save_policy_options(target: &SshTarget) -> PasswordOptions {
  PasswordOptions::new_generic_password(&save_policy_service(target), SAVE_POLICY_ACCOUNT)
}

fn service(target: &SshTarget) -> String {
  format!("{KEYCHAIN_SERVICE_PREFIX}.{}", scope_id(target))
}

fn save_policy_service(target: &SshTarget) -> String {
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
