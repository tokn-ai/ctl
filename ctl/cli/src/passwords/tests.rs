use super::*;

fn snapshot() -> Snapshot {
  inventory::build(
    credentials::Inventory {
      credentials: vec![credentials::StoredCredential {
        credential_id: format!("{}:{}", "a".repeat(64), "b".repeat(64)),
        scope_id: "a".repeat(64),
        name: "SSH password · work".into(),
        kind: credentials::CredentialKind::SshPassword,
        target: Some("work".into()),
        account: Some("alice".into()),
        key_name: None,
        created_at_ms: Some(1),
        updated_at_ms: Some(2),
      }],
      complete: true,
      warning: None,
      metadata_import_required: false,
    },
    identities::Inventory {
      identity_files: Vec::new(),
      complete: true,
      file_discovery_complete: true,
      warning: None,
      keychain_available: true,
      keychain_error: None,
      metadata_import_required: false,
    },
  )
}

#[test]
fn a_replaced_or_uncheckable_entry_cannot_be_removed_from_an_older_preview() {
  let original = snapshot();
  let entry = original.entries[0].clone();
  assert!(check_unchanged(&entry, &original).is_ok());
  let mut replaced = snapshot();
  replaced.entries[0].name = "Newly replaced entry".into();
  assert!(check_unchanged(&entry, &replaced).is_err());
  replaced.entries.clear();
  replaced.complete = false;
  assert!(check_unchanged(&entry, &replaced).is_err());
}
