use super::*;
use ctld_ipc::credentials::CredentialKind;
use sha2::{Digest as _, Sha256};

fn credential() -> StoredCredential {
  StoredCredential {
    credential_id: format!("{}:{}", "a".repeat(64), "b".repeat(64)),
    scope_id: "a".repeat(64),
    name: "SSH password · synthetic".into(),
    kind: CredentialKind::SshPassword,
    target: Some("synthetic".into()),
    account: Some("alice".into()),
    key_name: None,
    created_at_ms: Some(1000),
    updated_at_ms: Some(2000),
  }
}

fn record(service: &str, account: &str, comment: &str) -> Record {
  Record {
    attributes: HashMap::from([
      ("svce".into(), service.into()),
      ("acct".into(), account.into()),
      ("icmt".into(), comment.into()),
    ]),
    created_at_ms: None,
    updated_at_ms: None,
    secret: None,
  }
}

fn marker() -> Record {
  record(SERVICE, MARKER, "1")
}

#[test]
fn reimport_preserves_public_hint_only_for_the_same_protected_binding() {
  use base64::Engine as _;

  let path = "/fixture/private-key";
  let identity_id = super::super::digest(path.as_bytes());
  let mut blob = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
  blob.extend_from_slice(&[7; 32]);
  let prior = SavedIdentity {
    version: 1,
    path: path.into(),
    file_version: "a".repeat(64),
    key_type: "ssh-ed25519".into(),
    fingerprint: format!(
      "SHA256:{}",
      base64::engine::general_purpose::STANDARD_NO_PAD.encode(Sha256::digest(&blob))
    ),
    public_key: Some(format!(
      "ssh-ed25519 {}",
      base64::engine::general_purpose::STANDARD.encode(&blob)
    )),
  };
  let previous = HashMap::from([(identity_id.clone(), prior.clone())]);
  let mut source = prior.clone();
  source.public_key = None;
  let imported = imported_records(vec![
    record(
      super::super::identity::SERVICE,
      &identity_id,
      &serde_json::to_string(&source).unwrap(),
    ),
    record(
      SERVICE,
      &identity_account(&identity_id),
      &serde_json::to_string(&Entry::Identity {
        identity_id: identity_id.clone(),
        metadata: prior.clone(),
      })
      .unwrap(),
    ),
  ]);
  assert!(imported.complete);
  assert_eq!(
    imported.identities[&identity_id].public_key,
    prior.public_key
  );
  for field in ["file_version", "fingerprint", "key_type"] {
    let mut changed = source.clone();
    match field {
      "file_version" => changed.file_version = "b".repeat(64),
      "fingerprint" => changed.fingerprint = "SHA256:changed".into(),
      _ => changed.key_type = "ssh-rsa".into(),
    }
    let mut imported = HashMap::from([(identity_id.clone(), changed)]);
    preserve_public_hints(&mut imported, &previous);
    assert!(imported[&identity_id].public_key.is_none(), "{field}");
  }
}

fn indexed_credential() -> Record {
  let credential = credential();
  record(
    SERVICE,
    &credential_account(&credential.credential_id),
    &serde_json::to_string(&Entry::Credential { credential }).unwrap(),
  )
}

#[test]
fn only_completed_import_with_no_interrupted_mutations_is_complete() {
  assert!(project(Vec::new()).required());
  assert!(!project(vec![marker()]).required());
  assert!(
    project(vec![
      marker(),
      record(SERVICE, "pending:00000000-0000-4000-8000-000000000001", "1")
    ])
    .required()
  );
  assert!(project(vec![record(SERVICE, MARKER, "future-version")]).required());
  assert!(project(vec![marker(), record(SERVICE, "pending:malformed", "1")]).required());
}

#[test]
fn pending_import_preserves_known_names_without_claiming_complete_inventory() {
  let index = project(vec![
    indexed_credential(),
    marker(),
    record(SERVICE, "pending:00000000-0000-4000-8000-000000000001", "1"),
  ]);
  assert!(index.required());
  assert_eq!(index.credentials, [credential()]);
  assert_eq!(index.pending.len(), 1);
}

#[test]
fn missing_sources_are_omitted_and_unexpected_failures_make_inventory_partial() {
  let mut complete = true;
  assert!(!reconcile_presence(Ok(Presence::Missing), &mut complete));
  assert!(complete);
  assert!(reconcile_presence(Ok(Presence::Present), &mut complete));
  assert!(reconcile_presence(Ok(Presence::Protected), &mut complete));
  assert!(complete);
  assert!(!reconcile_presence(
    Err(ctl_keychain_client::Error(-34_018)),
    &mut complete
  ));
  assert!(!complete);
}

#[test]
fn sidecar_scope_and_account_must_match_validated_metadata() {
  let mut credential = credential();
  let account = credential_account(&credential.credential_id);
  credential.scope_id = "c".repeat(64);
  let index = project(vec![
    marker(),
    record(
      SERVICE,
      &account,
      &serde_json::to_string(&Entry::Credential { credential }).unwrap(),
    ),
  ]);
  assert!(index.required());
  assert_eq!(index.credentials, Vec::<StoredCredential>::new());
  let mut wrong_service = indexed_credential();
  wrong_service
    .attributes
    .insert("svce".into(), "other-service".into());
  assert!(project(vec![marker(), wrong_service]).required());
}

#[test]
fn imports_only_owned_credentials_and_retains_source_dates() {
  let expected = credential();
  let service = format!("{SERVICE_PREFIX}{}", expected.scope_id);
  let account = "b".repeat(64);
  let mut source = record(
    &service,
    &account,
    r#"{"version":1,"kind":"ssh_password","target":"synthetic","account":"alice","key_name":null}"#,
  );
  source.created_at_ms = expected.created_at_ms;
  source.updated_at_ms = expected.updated_at_ms;
  let imported = imported_records(vec![source, record("unrelated", "unrelated", "unrelated")]);
  assert!(imported.complete);
  assert_eq!(imported.credentials, [expected]);
  assert_eq!(
    imported.identities.keys().collect::<Vec<_>>(),
    Vec::<&String>::new()
  );
}

#[test]
fn malformed_owned_source_keeps_import_incomplete_without_inventing_key_binding() {
  assert!(
    !imported_records(vec![record(
      super::super::identity::SERVICE,
      &"a".repeat(64),
      "{}"
    )])
    .complete
  );
  assert!(
    !imported_records(vec![record(
      &format!("{SERVICE_PREFIX}{}", "a".repeat(64)),
      "invalid",
      "{}"
    )])
    .complete
  );
}

#[test]
fn identity_sidecar_requires_canonical_path_digest_and_exact_file_binding() {
  let path = "/synthetic/identity";
  let identity_id = format!("{:x}", Sha256::digest(path.as_bytes()));
  let metadata = SavedIdentity {
    version: 1,
    path: path.into(),
    file_version: "b".repeat(64),
    key_type: "ssh-ed25519".into(),
    fingerprint: "SHA256:synthetic".into(),
    public_key: None,
  };
  let comment = serde_json::to_string(&Entry::Identity {
    identity_id: identity_id.clone(),
    metadata: metadata.clone(),
  })
  .unwrap();
  let index = project(vec![
    marker(),
    record(SERVICE, &identity_account(&identity_id), &comment),
  ]);
  assert!(!index.required());
  assert_eq!(
    index.identities.get(&identity_id).unwrap().file_version,
    metadata.file_version
  );
  let imported = imported_records(vec![record(
    super::super::identity::SERVICE,
    &identity_id,
    &serde_json::to_string(&metadata).unwrap(),
  )]);
  assert!(imported.complete);
  assert_eq!(imported.credentials, Vec::<StoredCredential>::new());
  assert!(imported.identities.contains_key(&identity_id));
  let wrong = project(vec![
    marker(),
    record(SERVICE, &identity_account(&"c".repeat(64)), &comment),
  ]);
  assert!(wrong.required());
  assert_eq!(
    wrong.identities.keys().collect::<Vec<_>>(),
    Vec::<&String>::new()
  );
}

#[test]
fn partial_import_recovers_valid_rows_beside_malformed_owned_items() {
  let expected = credential();
  let service = format!("{SERVICE_PREFIX}{}", expected.scope_id);
  let source = record(
    &service,
    &"b".repeat(64),
    r#"{"version":1,"kind":"ssh_password","target":"synthetic","account":"alice","key_name":null}"#,
  );
  let imported = imported_records(vec![
    source,
    record(super::super::identity::SERVICE, &"a".repeat(64), "{}"),
  ]);
  assert!(!imported.complete);
  assert_eq!(imported.credentials.len(), 1);
  assert_eq!(
    imported.credentials[0].credential_id,
    expected.credential_id
  );
  assert_eq!(
    imported.identities.keys().collect::<Vec<_>>(),
    Vec::<&String>::new()
  );
}

#[test]
fn truncated_import_can_never_commit_a_complete_marker() {
  let records = (0..=MAX_ITEMS)
    .map(|_| record("unrelated", "item", "{}"))
    .collect();
  assert!(!imported_records(records).complete);
}
