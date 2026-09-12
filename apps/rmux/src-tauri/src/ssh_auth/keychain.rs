//! Touch ID-protected storage for SSH passwords and private-key passphrases.

use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{
  AccessControlOptions, PasswordOptions, delete_generic_password_options, generic_password,
  set_generic_password_options,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::Secrets;
use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

const KEYCHAIN_SERVICE_PREFIX: &str = "io.rmux.desktop.ssh";
const ITEM_NOT_FOUND: i32 = -25_300;
const MISSING_ENTITLEMENT: i32 = -34_018;

pub fn load(
  target: &ConnectionTargetDto,
  prompt: &str,
) -> CommandResult<Option<Zeroizing<String>>> {
  let mut options = password_options(target, prompt);
  options.use_protected_keychain();
  let bytes = match generic_password(options) {
    Ok(bytes) => Zeroizing::new(bytes),
    Err(error) if error.code() == ITEM_NOT_FOUND => return Ok(None),
    Err(error) => return Err(keychain_error("unlock", error)),
  };
  let mut bytes = bytes;
  let secret = String::from_utf8(std::mem::take(&mut *bytes)).map_err(|_| {
    CommandErrorDto::new(
      "ssh_credential_invalid",
      "The saved SSH credential is not valid UTF-8. Forget it and authenticate again.",
    )
  })?;
  Ok(Some(Zeroizing::new(secret)))
}

pub fn save(target: &ConnectionTargetDto, secrets: &Secrets) -> CommandResult<()> {
  let secrets = std::mem::take(&mut *secrets.lock().unwrap());
  for (prompt, secret) in secrets {
    let mut options = password_options(target, &prompt);
    options.use_protected_keychain();
    match delete_generic_password_options(options) {
      Ok(()) => {}
      Err(error) if error.code() == ITEM_NOT_FOUND => {}
      Err(error) => return Err(keychain_error("replace", error)),
    }
    let mut options = password_options(target, &prompt);
    options.use_protected_keychain();
    let access_control = SecAccessControl::create_with_protection(
      Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
      AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
    )
    .map_err(|error| keychain_error("protect", error))?;
    options.set_access_control(access_control);
    set_generic_password_options(secret.as_bytes(), options)
      .map_err(|error| keychain_error("save", error))?;
  }
  Ok(())
}

pub fn delete(target: &ConnectionTargetDto) -> CommandResult<()> {
  let mut options = ItemSearchOptions::new();
  options
    .class(ItemClass::generic_password())
    .service(&service(target))
    .ignore_legacy_keychains();
  match options.delete() {
    Ok(()) => Ok(()),
    Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
    Err(error) => Err(keychain_error("delete", error)),
  }
}

fn password_options(target: &ConnectionTargetDto, prompt: &str) -> PasswordOptions {
  PasswordOptions::new_generic_password(&service(target), &account(prompt))
}

fn account(prompt: &str) -> String {
  digest(prompt.as_bytes())
}

fn service(target: &ConnectionTargetDto) -> String {
  let destination = match target {
    ConnectionTargetDto::Ssh { destination, .. } => destination,
    ConnectionTargetDto::Local => "local",
  };
  format!(
    "{KEYCHAIN_SERVICE_PREFIX}.{}",
    digest(destination.as_bytes())
  )
}

fn digest(value: &[u8]) -> String {
  let bytes = Sha256::digest(value);
  let mut encoded = String::with_capacity(bytes.len() * 2);
  for byte in bytes {
    use std::fmt::Write as _;
    write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
  }
  encoded
}

fn keychain_error(action: &str, error: security_framework::base::Error) -> CommandErrorDto {
  let message = if error.code() == MISSING_ENTITLEMENT {
    "rmux must be signed with its application identifier entitlement before it can use the Touch ID-protected Keychain."
      .to_owned()
  } else {
    format!("Could not {action} the SSH credential in Keychain: {error}")
  };
  CommandErrorDto::new("ssh_keychain_failed", message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn service_names_do_not_expose_the_ssh_destination() {
    let target = ConnectionTargetDto::ssh("private.example.com");
    let service = service(&target);
    assert!(service.starts_with(KEYCHAIN_SERVICE_PREFIX));
    assert!(!service.contains("private.example.com"));
  }

  #[test]
  fn prompt_kinds_are_stored_as_distinct_accounts() {
    assert_ne!(
      account("Password:"),
      account("Enter passphrase for key '/key':")
    );
  }
}
