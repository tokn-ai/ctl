use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

struct SyntheticStore {
  records: Vec<Record>,
  scan_error: Option<i32>,
  cache_error: bool,
  reconciled: bool,
}

impl SyntheticStore {
  fn new(records: Vec<Record>) -> Self {
    Self {
      records,
      scan_error: None,
      cache_error: false,
      reconciled: false,
    }
  }
}

impl Store for SyntheticStore {
  fn scan(&mut self) -> Result<Vec<Record>, Error> {
    if let Some(code) = self.scan_error {
      return Err(ctl_keychain_client::Error(code).into());
    }
    Ok(std::mem::take(&mut self.records))
  }

  fn reconcile(&mut self, records: &[Record]) -> Result<(), Error> {
    assert!(records.iter().all(|record| record.secret.is_none()));
    self.reconciled = true;
    if self.cache_error {
      Err(invalid_metadata())
    } else {
      Ok(())
    }
  }
}

fn record(service: &str, account: &str, comment: &str) -> Record {
  Record {
    attributes: HashMap::from([
      ("svce".into(), service.into()),
      ("acct".into(), account.into()),
      ("icmt".into(), comment.into()),
    ]),
    created_at_ms: Some(1000),
    updated_at_ms: Some(2000),
    secret: None,
  }
}

fn credential(account: &str) -> Record {
  record(
    &format!("{SERVICE_PREFIX}{}", "a".repeat(64)),
    account,
    r#"{"version":1,"kind":"ssh_password","target":"fixture-host","account":"alice","key_name":null}"#,
  )
}

#[test]
fn fresh_and_unindexed_sources_are_complete_without_a_cache_marker() {
  let mut store = SyntheticStore::new(vec![credential(&"b".repeat(64))]);
  let inventory = run_with(&mut store).unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.warnings, Vec::<String>::new());
  assert!(store.reconciled);
  assert_eq!(inventory.entries.len(), 1);
  let entry = &inventory.entries[0];
  assert_eq!(entry.state, PasswordState::Saved);
  assert_eq!(entry.account.as_deref(), Some("alice"));
  assert_eq!(entry.created_at_ms, Some(1000));

  let inventory = run_with(&mut SyntheticStore::new(Vec::new())).unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.entries, Vec::<SavedPassword>::new());
  assert_eq!(inventory.warnings, Vec::<String>::new());
}

#[test]
fn malformed_and_legacy_metadata_remain_visible_without_changing_coverage() {
  let mut legacy = credential(&"b".repeat(64));
  legacy.attributes.remove("icmt");
  let mut malformed = credential(&"c".repeat(64));
  malformed
    .attributes
    .insert("icmt".into(), "not-json".into());
  let invalid = credential("not-a-valid-account");
  let bad_identity = record(identity::SERVICE, &"d".repeat(64), "{}");
  let inventory = run_with(&mut SyntheticStore::new(vec![
    legacy,
    malformed,
    invalid,
    bad_identity,
  ]))
  .unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.warnings, Vec::<String>::new());
  assert_eq!(inventory.entries.len(), 4);
  assert!(
    inventory
      .entries
      .iter()
      .all(|entry| entry.state == PasswordState::Unknown)
  );
  assert!(inventory.entries.iter().all(|entry| entry.detail.is_some()));
  assert!(
    inventory
      .entries
      .iter()
      .any(|entry| entry.id.starts_with("unknown:"))
  );
  assert!(
    inventory
      .entries
      .iter()
      .any(|entry| entry.id == "d".repeat(64))
  );
}

#[test]
fn unrelated_namespaces_and_policy_records_are_not_passwords() {
  let inventory = run_with(&mut SyntheticStore::new(vec![
    record("unrelated", "fixture", "{}"),
    record(
      "dev.tokn-ai.ctl.ctld.ssh-save-policy.fixture",
      "policy",
      "{}",
    ),
    record(index::SERVICE, index::MARKER, "1"),
  ]))
  .unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.entries, Vec::<SavedPassword>::new());
}

#[test]
fn cache_refresh_failure_does_not_hide_an_exhaustive_inventory() {
  let mut store = SyntheticStore::new(vec![credential(&"b".repeat(64))]);
  store.cache_error = true;
  let inventory = run_with(&mut store).unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.entries.len(), 1);
  assert_eq!(inventory.warnings, [CACHE_WARNING]);
}

#[test]
fn failed_or_canceled_scan_cannot_claim_an_empty_inventory() {
  for code in [
    -128,
    -25_308,
    -25_291,
    ctl_keychain_client::ATTRIBUTE_SCAN_LIMIT,
  ] {
    let mut store = SyntheticStore::new(Vec::new());
    store.scan_error = Some(code);
    assert_eq!(run_with(&mut store).unwrap_err().0.code(), code);
    assert!(!store.reconciled);
  }
}

#[test]
fn discovery_never_accepts_secret_values_or_ambiguous_selectors() {
  let mut source = credential(&"b".repeat(64));
  source.secret = Some(zeroize::Zeroizing::new(b"synthetic-never-return".to_vec()));
  let mut store = SyntheticStore::new(vec![source]);
  assert!(run_with(&mut store).is_err());
  assert!(!store.reconciled);
  let mut store = SyntheticStore::new(vec![
    credential(&"b".repeat(64)),
    credential(&"b".repeat(64)),
  ]);
  assert_eq!(
    run_with(&mut store).unwrap_err().0.code(),
    ctl_keychain_client::ATTRIBUTE_SCAN_CONFLICT
  );
  assert!(!store.reconciled);
}

#[test]
fn source_items_beyond_the_old_converter_page_are_all_listed() {
  let records = (0..=credential_metadata::MAX_SEARCH_ITEMS)
    .map(|index| credential(&format!("{index:064x}")))
    .collect::<Vec<_>>();
  let count = records.len();
  let inventory = run_with(&mut SyntheticStore::new(records)).unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.entries.len(), count);
}

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path =
      std::env::temp_dir().join(format!("ctld-discovery-fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn write(&self) -> String {
    let path = self.0.join("synthetic-key");
    std::fs::write(&path, "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMBcwAwYBKgQQeHh4eHh4eHh4eHh4eHh4eA==\n-----END ENCRYPTED PRIVATE KEY-----\n").unwrap();
    path.to_string_lossy().into_owned()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn saved_identity_file_binding_is_inspected_without_unlocking_the_key() {
  let fixture = Fixture::new();
  let path = fixture.write();
  let snapshot = inspect_path(&path).unwrap();
  let metadata = SavedIdentity {
    version: 1,
    path: snapshot.path.clone(),
    file_version: snapshot.file_version,
    key_type: "ssh-ed25519".into(),
    fingerprint: "SHA256:synthetic".into(),
    public_key: None,
  };
  let source = || {
    record(
      identity::SERVICE,
      &snapshot.identity_id,
      &serde_json::to_string(&metadata).unwrap(),
    )
  };
  let inventory = run_with(&mut SyntheticStore::new(vec![source()])).unwrap();
  assert!(inventory.complete);
  let entry = &inventory.entries[0];
  assert_eq!(entry.state, PasswordState::Saved);
  assert_eq!(entry.file_state, Some(FileState::Ready));
  assert_eq!(entry.encrypted, Some(true));
  assert_eq!(entry.fingerprint.as_deref(), Some("SHA256:synthetic"));
  std::fs::remove_file(path).unwrap();
  let inventory = run_with(&mut SyntheticStore::new(vec![source()])).unwrap();
  assert!(inventory.complete);
  assert_eq!(inventory.entries[0].state, PasswordState::FileChanged);
  assert_eq!(inventory.entries[0].file_state, Some(FileState::Missing));
  assert!(inventory.entries[0].key_type.is_none());
  assert!(inventory.entries[0].fingerprint.is_none());
}

#[test]
fn unknown_identifiers_are_stable_under_attribute_iteration_order() {
  let first = credential("invalid");
  let mut second = credential("invalid");
  let mut attributes = first
    .attributes
    .iter()
    .map(|(key, value)| (key.clone(), value.clone()))
    .collect::<Vec<_>>();
  attributes.reverse();
  second.attributes = attributes.into_iter().collect();
  assert_eq!(unknown_id(&first), unknown_id(&second));
}
