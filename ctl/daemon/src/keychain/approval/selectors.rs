//! Passive selector discovery for existing shells after a daemon restart.

use std::collections::HashSet;

use ctl_ipc::SshTarget;
use ctl_ipc::credentials::{CredentialKind, StoredCredential, lookup_scope_ids};

use super::super::{Error, SERVICE_PREFIX, availability, index};
use crate::credential_metadata;

/// The caller must validate current SSH/trust scope and an already-open shell.
/// Only validated, present metadata contributes selectors. Unknown or missing
/// metadata and legacy key prompts cannot establish an exact identity binding;
/// they retain ordinary native SSH authentication instead of being imported or
/// probed interactively here.
pub(crate) async fn discover(target: &SshTarget) -> Result<Vec<String>, Error> {
  let scopes = lookup_scope_ids(target);
  let mut selectors = tokio::task::spawn_blocking(move || {
    availability()?;
    let (credentials, _, _) = index::list()?;
    Ok::<_, Error>(password_selectors(&scopes, &credentials))
  })
  .await
  .map_err(|_| Error::ApprovalUnavailable)??;
  let identities = crate::identity_connection::wake_bindings(target).await?;
  selectors.extend(identities.into_iter().map(|(identity_id, file_version)| {
    format!(
      "{}:{identity_id}:{file_version}",
      super::super::identity::SERVICE
    )
  }));
  Ok(selectors)
}

fn password_selectors(scopes: &[String], credentials: &[StoredCredential]) -> Vec<String> {
  let mut selectors = Vec::new();
  let mut accounts = HashSet::new();
  for scope in scopes {
    for credential in credentials {
      let Some((saved_scope, account)) =
        credential_metadata::item_identity(&credential.credential_id)
      else {
        continue;
      };
      if saved_scope != scope || credential.scope_id != saved_scope || !accounts.insert(account) {
        continue;
      }
      // A present current item shadows the same legacy prompt, even when its
      // metadata is insufficient for automatic authorization.
      if credential.kind != CredentialKind::SshPassword || credential.target.is_none() {
        continue;
      }
      selectors.push(format!("{SERVICE_PREFIX}{saved_scope}:{account}"));
    }
  }
  selectors.sort();
  selectors
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(scope: &str, account: &str, kind: CredentialKind) -> StoredCredential {
    StoredCredential {
      credential_id: format!("{scope}:{account}"),
      scope_id: scope.into(),
      name: "Saved SSH password".into(),
      kind,
      target: Some("fixture.invalid".into()),
      account: None,
      key_name: None,
      created_at_ms: None,
      updated_at_ms: None,
    }
  }

  #[test]
  fn current_items_shadow_legacy_items_without_authorizing_other_scopes() {
    let current = "1".repeat(64);
    let legacy = "2".repeat(64);
    let unrelated = "3".repeat(64);
    let password = "a".repeat(64);
    let other_password = "b".repeat(64);
    let credentials = vec![
      record(&legacy, &password, CredentialKind::SshPassword),
      record(&unrelated, &other_password, CredentialKind::SshPassword),
      record(&current, &password, CredentialKind::SshPassword),
      record(&legacy, &other_password, CredentialKind::SshPassword),
    ];
    assert_eq!(
      password_selectors(&[current.clone(), legacy.clone()], &credentials),
      vec![
        format!("{SERVICE_PREFIX}{current}:{password}"),
        format!("{SERVICE_PREFIX}{legacy}:{other_password}"),
      ]
    );
  }

  #[test]
  fn unknown_metadata_and_unbound_key_prompts_do_not_fall_back_to_legacy() {
    let current = "1".repeat(64);
    let legacy = "2".repeat(64);
    let unknown = "a".repeat(64);
    let key = "b".repeat(64);
    let generic = "c".repeat(64);
    let mut unknown_record = record(&current, &unknown, CredentialKind::SshPassword);
    unknown_record.target = None;
    let credentials = vec![
      unknown_record,
      record(&current, &key, CredentialKind::SshKeyPassphrase),
      record(&current, &generic, CredentialKind::SshCredential),
      record(&legacy, &unknown, CredentialKind::SshPassword),
      record(&legacy, &key, CredentialKind::SshPassword),
      record(&legacy, &generic, CredentialKind::SshPassword),
    ];
    assert_eq!(
      password_selectors(&[current, legacy], &credentials),
      Vec::<String>::new()
    );
  }

  #[test]
  fn malformed_and_mismatched_scope_ids_are_not_selectors() {
    let scope = "1".repeat(64);
    let mut mismatched = record(&scope, &"a".repeat(64), CredentialKind::SshPassword);
    mismatched.scope_id = "2".repeat(64);
    assert_eq!(
      password_selectors(
        std::slice::from_ref(&scope),
        &[
          mismatched,
          record(&scope, "invalid", CredentialKind::SshPassword),
        ],
      ),
      Vec::<String>::new()
    );
  }
}
