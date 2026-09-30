//! A separate, exact identity-file namespace. List reads attributes only.

use super::{Error, ITEM_NOT_FOUND};
use crate::identities::{IdentityError, IdentitySnapshot, SavedIdentity, VerifiedIdentity};
use core_foundation::data::CFData;
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::item::{
  ItemClass, ItemSearchOptions, ItemUpdateOptions, ItemUpdateValue, update_item,
};
use security_framework::passwords::{
  AccessControlOptions, PasswordOptions, generic_password, set_generic_password_options,
};
use std::collections::HashMap;
use zeroize::Zeroizing;

const SERVICE: &str = "io.rmux.desktop.ctld.ssh-identity";
const MAX_ITEMS: usize = 512;

pub(crate) fn list() -> Result<(HashMap<String, SavedIdentity>, bool), IdentityError> {
  let mut options = ItemSearchOptions::new();
  options
    .class(ItemClass::generic_password())
    .service(SERVICE)
    .ignore_legacy_keychains()
    .load_attributes(true)
    .load_data(false)
    .load_refs(false)
    .limit(513);
  let results = match options.search() {
    Ok(results) => results,
    Err(error) if error.code() == ITEM_NOT_FOUND => Vec::new(),
    Err(error) => return Err(map_error(error, IdentityError::ListFailed)),
  };
  let mut complete = results.len() <= MAX_ITEMS;
  let mut entries = HashMap::new();
  for result in results.iter().take(MAX_ITEMS) {
    let Some(attributes) = result.simplify_dict() else {
      complete = false;
      continue;
    };
    let (Some(account), Some(comment)) = (attributes.get("acct"), attributes.get("icmt")) else {
      complete = false;
      continue;
    };
    let Ok(metadata) = serde_json::from_str::<SavedIdentity>(comment) else {
      complete = false;
      continue;
    };
    if metadata.valid(account) {
      entries.insert(account.clone(), metadata);
    } else {
      complete = false;
    }
  }
  Ok((entries, complete))
}

pub(crate) fn load(
  snapshot: &IdentitySnapshot,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  let (entries, _) = list()?;
  let Some(metadata) = entries.get(&snapshot.identity_id) else {
    return Ok(None);
  };
  if metadata.file_version != snapshot.file_version {
    return Ok(None);
  }
  let mut options = PasswordOptions::new_generic_password(SERVICE, &snapshot.identity_id);
  options.use_protected_keychain();
  let mut bytes = Zeroizing::new(match generic_password(options) {
    Ok(bytes) => bytes,
    Err(error) if error.code() == ITEM_NOT_FOUND => return Ok(None),
    Err(error) => return Err(map_error(error, IdentityError::KeychainUnavailable)),
  });
  match String::from_utf8(std::mem::take(&mut *bytes)) {
    Ok(secret) => Ok(Some(Zeroizing::new(secret))),
    Err(error) => {
      let _bytes = Zeroizing::new(error.into_bytes());
      Err(IdentityError::KeychainUnavailable)
    }
  }
}

pub(crate) fn save(
  snapshot: &IdentitySnapshot,
  verified: &VerifiedIdentity,
  passphrase: &str,
) -> Result<(), IdentityError> {
  let metadata = SavedIdentity {
    version: 1,
    path: snapshot.path.clone(),
    file_version: snapshot.file_version.clone(),
    key_type: verified.key_type.clone(),
    fingerprint: verified.fingerprint.clone(),
  };
  let comment = serde_json::to_string(&metadata).map_err(|_| IdentityError::SaveFailed)?;
  let label = format!(
    "SSH identity: {}",
    std::path::Path::new(&snapshot.path)
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
  );
  let mut query = ItemSearchOptions::new();
  query
    .class(ItemClass::generic_password())
    .service(SERVICE)
    .account(&snapshot.identity_id)
    .ignore_legacy_keychains();
  let mut update = ItemUpdateOptions::new();
  update
    .set_value(ItemUpdateValue::Data(CFData::from_buffer(
      passphrase.as_bytes(),
    )))
    .set_comment(&comment)
    .set_label(&label);
  // Replace the secret and binding together, preserving existing protection.
  // A denied update leaves the previous credential intact.
  match update_item(&query, &update) {
    Ok(()) => return Ok(()),
    Err(error) if error.code() == ITEM_NOT_FOUND => {}
    Err(error) => return Err(map_error(error, IdentityError::SaveFailed)),
  }
  let mut options = PasswordOptions::new_generic_password(SERVICE, &snapshot.identity_id);
  options.use_protected_keychain();
  let control = SecAccessControl::create_with_protection(
    Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
    AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
  )
  .map_err(|error| map_error(error, IdentityError::SaveFailed))?;
  options.set_access_control(control);
  options.set_label(&label);
  options.set_description("rmux SSH identity passphrase");
  options.set_comment(&comment);
  set_generic_password_options(passphrase.as_bytes(), options)
    .map_err(|error| map_error(error, IdentityError::SaveFailed))
}

pub(crate) fn forget(identity_id: &str) -> Result<(), IdentityError> {
  if identity_id.len() != 64
    || !identity_id
      .bytes()
      .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
  {
    return Err(IdentityError::InvalidRequest);
  }
  let mut options = ItemSearchOptions::new();
  options
    .class(ItemClass::generic_password())
    .service(SERVICE)
    .account(identity_id)
    .ignore_legacy_keychains();
  match options.delete() {
    Ok(()) => Ok(()),
    Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
    Err(error) => Err(map_error(error, IdentityError::ForgetFailed)),
  }
}

fn map_error(error: security_framework::base::Error, fallback: IdentityError) -> IdentityError {
  if Error(error).is_missing_entitlement() {
    IdentityError::KeychainUnavailable
  } else if matches!(error.code(), -25_308 | -25_315 | -25_293 | -128 | -25_291) {
    IdentityError::KeychainLocked
  } else {
    fallback
  }
}
