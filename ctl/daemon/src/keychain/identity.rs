//! Exact identity-file secrets and their separately readable metadata index.

use super::{Error, index, purpose};
use crate::identities::{IdentityError, IdentitySnapshot, SavedIdentity, VerifiedIdentity};
use ctl_keychain_client::{Authentication, Presence, Query, Write};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::Zeroizing;

pub(super) const SERVICE: &str = "dev.tokn-ai.ctl.ctld.ssh-identity";
const MAX_COMMENT_BYTES: usize = 16 * 1024;

pub(crate) fn saved_metadata(
  snapshot: &IdentitySnapshot,
) -> Result<Option<SavedIdentity>, IdentityError> {
  let metadata = index::identity_metadata(&snapshot.identity_id)
    .map_err(|error| map_error(error, IdentityError::ListFailed))?;
  // The readable index establishes an unlocked context. An exact protected
  // match is present, but its value and binding still need authenticated read
  // and local verification later, when signing is actually requested.
  let presence = ctl_keychain_client::exists(SERVICE, &snapshot.identity_id)
    .map_err(|error| map_error(error.into(), IdentityError::ListFailed))?;
  let Some(metadata) = present_metadata(metadata, presence)? else {
    return Ok(None);
  };
  metadata.check_binding(
    &snapshot.identity_id,
    &snapshot.path,
    &snapshot.file_version,
  )?;
  Ok(Some(metadata))
}

fn present_metadata(
  metadata: Option<SavedIdentity>,
  presence: Presence,
) -> Result<Option<SavedIdentity>, IdentityError> {
  match presence {
    Presence::Missing => Ok(None),
    // An existing secret without valid readable metadata is not an unsaved key.
    // Preserve native authentication without retrieving the protected secret.
    Presence::Present | Presence::Protected => metadata.map(Some).ok_or(IdentityError::ListFailed),
  }
}

pub(crate) fn list() -> Result<(HashMap<String, SavedIdentity>, bool), IdentityError> {
  super::availability().map_err(|error| map_error(error, IdentityError::ListFailed))?;
  let (_, entries, complete) =
    index::list().map_err(|error| map_error(error, IdentityError::ListFailed))?;
  Ok((entries, complete))
}

pub(crate) fn load(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
  canceled: Option<&AtomicBool>,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  load_inner(snapshot, context, canceled, None)
}

pub(crate) fn load_for_connection(
  snapshot: &IdentitySnapshot,
  context: &str,
  canceled: &AtomicBool,
  authorization: &super::approval::Attempt,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  crate::identities::ensure_current(snapshot)?;
  load_inner(snapshot, Some(context), Some(canceled), Some(authorization))
}

fn load_inner(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
  canceled: Option<&AtomicBool>,
  authorization: Option<&super::approval::Attempt>,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation = Operation::start(
    "a56e822c-e651-4aeb-9b21-2d78d4439ea6",
    Event::CredentialRead,
    Some(&snapshot.identity_id),
  );
  let result = load_unrecorded(snapshot, context, canceled, authorization);
  operation.finish(
    if matches!(&result, Ok(None)) {
      Outcome::Missing
    } else if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|error| error.code()),
    None,
  );
  result
}

fn load_unrecorded(
  snapshot: &IdentitySnapshot,
  context: Option<&str>,
  canceled: Option<&AtomicBool>,
  authorization: Option<&super::approval::Attempt>,
) -> Result<Option<Zeroizing<String>>, IdentityError> {
  let reason = purpose::identity("Read", &snapshot.path, context);
  let records = with_authentication(
    canceled,
    || {
      super::operation::acquire()
        .map_err(|error| map_error(error, IdentityError::KeychainUnavailable))
    },
    |guard| {
      let read = |authentication: Authentication<'_>| {
        ctl_keychain_client::search(&Query {
          service: Some(SERVICE),
          account: Some(&snapshot.identity_id),
          limit: 1,
          secret: true,
          authentication,
        })
        .map_err(super::Error::from)
        .map(|records| (!records.is_empty()).then_some(records))
      };
      let records = if let Some(authorization) = authorization {
        authorization.read(
          guard,
          &format!(
            "{SERVICE}:{}:{}",
            snapshot.identity_id, snapshot.file_version
          ),
          &reason,
          read,
        )
      } else {
        read(Authentication::Allow { reason: &reason })
      };
      records.map_err(|error| map_error(error, IdentityError::KeychainUnavailable))
    },
  )?
  .unwrap_or_default();
  let Some(record) = records.into_iter().next() else {
    return Ok(None);
  };
  // Both the protected binding and secret come from this exact same query.
  // A sidecar is display metadata and never authorizes reuse of a secret.
  check_binding(&record.attributes, snapshot)?;
  super::secret_string(record)
    .map(Some)
    .map_err(|error| map_error(error, IdentityError::KeychainUnavailable))
}

fn with_authentication<T, Guard>(
  canceled: Option<&AtomicBool>,
  acquire: impl FnOnce() -> Result<Guard, IdentityError>,
  query: impl FnOnce(&Guard) -> Result<T, IdentityError>,
) -> Result<T, IdentityError> {
  let check = || {
    if canceled.is_some_and(|canceled| canceled.load(Ordering::Acquire)) {
      Err(IdentityError::UnlockFailed)
    } else {
      Ok(())
    }
  };
  check()?;
  let operation = acquire()?;
  check()?;
  query(&operation)
}

fn check_binding(
  attributes: &HashMap<String, String>,
  snapshot: &IdentitySnapshot,
) -> Result<(), IdentityError> {
  check_protected_binding(
    attributes,
    &snapshot.identity_id,
    &snapshot.path,
    &snapshot.file_version,
  )
}

fn check_protected_binding(
  attributes: &HashMap<String, String>,
  identity_id: &str,
  path: &str,
  file_version: &str,
) -> Result<(), IdentityError> {
  if attributes.get("acct").map(String::as_str) != Some(identity_id)
    || attributes.get("svce").map(String::as_str) != Some(SERVICE)
  {
    return Err(IdentityError::ListFailed);
  }
  let comment = attributes
    .get("icmt")
    .filter(|value| value.len() <= MAX_COMMENT_BYTES)
    .ok_or(IdentityError::ListFailed)?;
  let metadata: SavedIdentity =
    serde_json::from_str(comment).map_err(|_| IdentityError::ListFailed)?;
  metadata.check_binding(identity_id, path, file_version)
}

pub(crate) fn save(
  snapshot: &IdentitySnapshot,
  verified: &VerifiedIdentity,
  passphrase: &str,
) -> Result<(), IdentityError> {
  use ctl_core::observability::{Event, Operation, Outcome};
  let operation = Operation::start(
    "c77305d2-64f4-4998-b7a0-44b764b53a3a",
    Event::CredentialSave,
    Some(&snapshot.identity_id),
  );
  let result = save_unrecorded(snapshot, verified, passphrase);
  operation.finish(
    if result.is_ok() {
      Outcome::Succeeded
    } else {
      Outcome::Failed
    },
    result.as_ref().err().map(|error| error.code()),
    None,
  );
  result
}

fn save_unrecorded(
  snapshot: &IdentitySnapshot,
  verified: &VerifiedIdentity,
  passphrase: &str,
) -> Result<(), IdentityError> {
  let _operation =
    super::operation::acquire().map_err(|error| map_error(error, IdentityError::SaveFailed))?;
  let mut metadata = SavedIdentity {
    version: 1,
    path: snapshot.path.clone(),
    file_version: snapshot.file_version.clone(),
    key_type: verified.key_type.clone(),
    fingerprint: verified.fingerprint.clone(),
    public_key: Some(verified.public_key.clone()),
  };
  let comment = metadata_comment(&mut metadata)?;
  let label = format!(
    "SSH identity: {}",
    std::path::Path::new(&snapshot.path)
      .file_name()
      .unwrap_or_default()
      .to_string_lossy()
  );
  let reason = purpose::identity("Save", &snapshot.path, None);
  let pending =
    index::begin_secret_mutation().map_err(|error| map_error(error, IdentityError::SaveFailed))?;
  ctl_keychain_client::upsert(&Write {
    service: SERVICE,
    account: &snapshot.identity_id,
    label: &label,
    comment: &comment,
    data: passphrase.as_bytes(),
    user_presence: true,
    authentication: Authentication::Allow { reason: &reason },
  })
  .map_err(|error| map_error(error.into(), IdentityError::SaveFailed))?;
  index::save_identity(&snapshot.identity_id, &metadata)
    .map_err(|error| map_error(error, IdentityError::SaveFailed))?;
  index::finish_mutation(&pending).map_err(|error| map_error(error, IdentityError::SaveFailed))
}

fn metadata_comment(metadata: &mut SavedIdentity) -> Result<String, IdentityError> {
  let identity_id = super::digest(metadata.path.as_bytes());
  if !metadata.valid(&identity_id) {
    // A large public key is only an optional hint; never make an otherwise
    // valid saved passphrase impossible to read back because of its size.
    metadata.public_key = None;
  }
  if !metadata.valid(&identity_id) {
    return Err(IdentityError::SaveFailed);
  }
  // Keep protected binding comments readable by already-running older ctld
  // processes, whose schema rejects unknown fields. Hints live in the index.
  let mut protected = metadata.clone();
  protected.public_key = None;
  let comment = serde_json::to_string(&protected).map_err(|_| IdentityError::SaveFailed)?;
  if comment.len() > MAX_COMMENT_BYTES {
    return Err(IdentityError::SaveFailed);
  }
  Ok(comment)
}

pub(crate) fn forget(identity_id: &str) -> Result<(), IdentityError> {
  let _operation =
    super::operation::acquire().map_err(|error| map_error(error, IdentityError::ForgetFailed))?;
  if identity_id.len() != 64
    || !identity_id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
  {
    return Err(IdentityError::InvalidRequest);
  }
  super::availability().map_err(|error| map_error(error, IdentityError::ForgetFailed))?;
  let presence = ctl_keychain_client::exists(SERVICE, identity_id)
    .map_err(|error| map_error(error.into(), IdentityError::ForgetFailed))?;
  if presence == Presence::Missing {
    return Err(IdentityError::InvalidRequest);
  }
  // Unknown metadata must not hide an owned secret from explicit exact-item
  // removal. Authentication and the validated namespace still protect deletion.
  let cached = index::list()
    .ok()
    .and_then(|(_, mut entries, _)| entries.remove(identity_id));
  let reason = cached.as_ref().map_or_else(
    || {
      format!(
        "Remove saved SSH key passphrase {} from Keychain",
        &identity_id[..12]
      )
    },
    |metadata| purpose::identity("Remove the saved", &metadata.path, None),
  );
  let pending = index::begin_secret_mutation()
    .map_err(|error| map_error(error, IdentityError::ForgetFailed))?;
  ctl_keychain_client::delete(
    SERVICE,
    Some(identity_id),
    Authentication::Allow { reason: &reason },
  )
  .map_err(|error| map_error(error.into(), IdentityError::ForgetFailed))?;
  index::forget_identity(identity_id)
    .map_err(|error| map_error(error, IdentityError::ForgetFailed))?;
  index::finish_mutation(&pending).map_err(|error| map_error(error, IdentityError::ForgetFailed))
}

pub(crate) fn map_error(error: Error, fallback: IdentityError) -> IdentityError {
  if error.is_busy() {
    IdentityError::KeychainBusy
  } else if error.is_missing_entitlement() {
    IdentityError::KeychainMissingEntitlement
  } else if error.is_unavailable() {
    IdentityError::KeychainUnavailable
  } else if error.is_locked() {
    IdentityError::KeychainLocked
  } else {
    fallback
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn access_errors_preserve_their_fallback_reason() {
    assert!(matches!(
      map_error(
        ctl_keychain_client::Error(super::super::MISSING_ENTITLEMENT).into(),
        IdentityError::ListFailed
      ),
      IdentityError::KeychainMissingEntitlement
    ));
    assert!(matches!(
      map_error(
        ctl_keychain_client::Error(-25_291).into(),
        IdentityError::ListFailed
      ),
      IdentityError::KeychainUnavailable
    ));
    for code in [-25_308, -25_315, -25_293, -128] {
      assert!(matches!(
        map_error(
          ctl_keychain_client::Error(code).into(),
          IdentityError::ListFailed
        ),
        IdentityError::KeychainLocked
      ));
    }
    assert!(matches!(
      map_error(
        ctl_keychain_client::Error(super::super::operation::BUSY).into(),
        IdentityError::ListFailed
      ),
      IdentityError::KeychainBusy
    ));
    assert!(matches!(
      map_error(
        ctl_keychain_client::Error(-50).into(),
        IdentityError::ListFailed
      ),
      IdentityError::ListFailed
    ));
  }

  #[test]
  fn canceled_unlock_never_starts_or_authenticates_after_waiting_for_the_lock() {
    struct Guard<'a>(&'a AtomicBool);
    impl Drop for Guard<'_> {
      fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
      }
    }
    let canceled = AtomicBool::new(true);
    let result: Result<(), _> = with_authentication(
      Some(&canceled),
      || -> Result<(), IdentityError> { panic!("canceled attempt acquired operation lock") },
      |()| panic!("canceled attempt authenticated"),
    );
    assert!(matches!(result, Err(IdentityError::UnlockFailed)));

    let released = AtomicBool::new(false);
    canceled.store(false, Ordering::Release);
    let result: Result<(), _> = with_authentication(
      Some(&canceled),
      || {
        // The attempt disappears while another interactive operation owns
        // the lock; acquisition subsequently returns to this blocked worker.
        canceled.store(true, Ordering::Release);
        Ok(Guard(&released))
      },
      |_| panic!("queued cancellation opened an authentication prompt"),
    );
    assert!(matches!(result, Err(IdentityError::UnlockFailed)));
    assert!(released.load(Ordering::Acquire));
    assert_eq!(with_authentication(None, || Ok(()), |()| Ok(7)).unwrap(), 7);
  }

  fn attributes() -> (String, SavedIdentity, HashMap<String, String>) {
    let metadata = SavedIdentity {
      version: 1,
      path: "/fixture/private-key".into(),
      file_version: "a".repeat(64),
      key_type: "ssh-ed25519".into(),
      fingerprint: "SHA256:fixture".into(),
      public_key: None,
    };
    let id = super::super::digest(metadata.path.as_bytes());
    let values = HashMap::from([
      ("svce".into(), SERVICE.into()),
      ("acct".into(), id.clone()),
      ("icmt".into(), serde_json::to_string(&metadata).unwrap()),
    ]);
    (id, metadata, values)
  }

  #[test]
  fn only_exact_protected_binding_authorizes_using_returned_secret() {
    let (id, metadata, values) = attributes();
    assert!(check_protected_binding(&values, &id, &metadata.path, &metadata.file_version).is_ok());
    assert!(matches!(
      check_protected_binding(&values, &id, &metadata.path, &"b".repeat(64)),
      Err(IdentityError::FileChanged)
    ));
    assert!(matches!(
      check_protected_binding(
        &values,
        &id,
        "/fixture/replaced-key",
        &metadata.file_version
      ),
      Err(IdentityError::ListFailed)
    ));
    assert!(matches!(
      check_protected_binding(
        &values,
        &"b".repeat(64),
        &metadata.path,
        &metadata.file_version
      ),
      Err(IdentityError::ListFailed)
    ));
  }

  #[test]
  fn wrong_namespace_or_malformed_metadata_is_not_reused() {
    let (id, metadata, mut values) = attributes();
    values.insert("svce".into(), "another-service".into());
    assert!(matches!(
      check_protected_binding(&values, &id, &metadata.path, &metadata.file_version),
      Err(IdentityError::ListFailed)
    ));
    values.insert("svce".into(), SERVICE.into());
    values.insert("icmt".into(), "not metadata".into());
    assert!(matches!(
      check_protected_binding(&values, &id, &metadata.path, &metadata.file_version),
      Err(IdentityError::ListFailed)
    ));
  }

  #[test]
  fn present_protected_entries_without_readable_metadata_are_not_reported_as_unsaved() {
    let (_, metadata, _) = attributes();
    for presence in [Presence::Present, Presence::Protected] {
      assert!(matches!(
        present_metadata(None, presence),
        Err(IdentityError::ListFailed)
      ));
      assert!(
        present_metadata(Some(metadata.clone()), presence)
          .unwrap()
          .is_some()
      );
    }
    assert!(present_metadata(None, Presence::Missing).unwrap().is_none());
    assert!(
      present_metadata(Some(metadata), Presence::Missing)
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn oversized_optional_public_hint_does_not_break_saved_passphrase_binding() {
    let (identity_id, mut metadata, mut values) = attributes();
    metadata.public_key = Some("x".repeat(MAX_COMMENT_BYTES));
    let comment = metadata_comment(&mut metadata).unwrap();
    assert!(comment.len() <= MAX_COMMENT_BYTES);
    assert!(metadata.public_key.is_none());
    values.insert("icmt".into(), comment);
    assert!(
      check_protected_binding(
        &values,
        &identity_id,
        &metadata.path,
        &metadata.file_version
      )
      .is_ok()
    );
  }

  #[test]
  fn public_hint_is_saved_only_in_sidecar_not_the_legacy_protected_binding() {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};

    let (identity_id, mut metadata, mut values) = attributes();
    let mut blob = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    blob.extend_from_slice(&[7; 32]);
    let public = format!(
      "ssh-ed25519 {}",
      base64::engine::general_purpose::STANDARD.encode(&blob)
    );
    metadata.fingerprint = format!(
      "SHA256:{}",
      base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    );
    metadata.public_key = Some(public);
    let comment = metadata_comment(&mut metadata).unwrap();
    assert!(!comment.contains("public_key"));
    assert!(metadata.public_key.is_some());
    values.insert("icmt".into(), comment);
    assert!(
      check_protected_binding(
        &values,
        &identity_id,
        &metadata.path,
        &metadata.file_version
      )
      .is_ok()
    );
  }
}
