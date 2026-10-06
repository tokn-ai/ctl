use super::*;
use std::collections::HashMap;

fn record(service: &str, account: &str) -> Record {
  Record {
    attributes: HashMap::from([
      ("svce".into(), service.into()),
      ("acct".into(), account.into()),
      // Broken/old display metadata must not hide a safely identifiable secret.
      ("icmt".into(), "not metadata".into()),
    ]),
    created_at_ms: None,
    updated_at_ms: None,
    secret: None,
  }
}

fn ssh_service(scope: char) -> String {
  format!("{SERVICE_PREFIX}{}", scope.to_string().repeat(64))
}

struct FakeStore {
  records: Option<Vec<Record>>,
  items: BTreeSet<(String, String)>,
  metadata: BTreeSet<String>,
  deletes: Vec<(String, String)>,
  began: bool,
  reset: bool,
  failure: Option<Failure>,
}

enum Failure {
  Scan,
  Delete(usize),
  Reset,
}

impl FakeStore {
  fn new(records: Vec<Record>) -> Self {
    let items = records
      .iter()
      .filter_map(|record| {
        Some((
          record.attributes.get("svce")?.clone(),
          record.attributes.get("acct")?.clone(),
        ))
      })
      .collect();
    Self {
      records: Some(records),
      items,
      metadata: BTreeSet::from(["old indexed item".into()]),
      deletes: Vec::new(),
      began: false,
      reset: false,
      failure: None,
    }
  }
}

impl Store for FakeStore {
  fn scan(&mut self) -> Result<Vec<Record>, Error> {
    if matches!(self.failure, Some(Failure::Scan)) {
      return Err(invalid_metadata());
    }
    Ok(self.records.take().unwrap())
  }

  fn begin_mutation(&mut self) -> Result<(), Error> {
    self.began = true;
    self.metadata.insert("pending".into());
    Ok(())
  }

  fn delete(&mut self, item: &Item) -> Result<(), Error> {
    let selector = (item.service.clone(), item.account.clone());
    self.deletes.push(selector.clone());
    if matches!(self.failure, Some(Failure::Delete(index)) if index == self.deletes.len()) {
      return Err(invalid_metadata());
    }
    assert!(self.items.remove(&selector));
    Ok(())
  }

  fn reset_empty(&mut self) -> Result<(), Error> {
    if matches!(self.failure, Some(Failure::Reset)) {
      return Err(invalid_metadata());
    }
    self.metadata = BTreeSet::from(["import-complete".into()]);
    self.reset = true;
    Ok(())
  }
}

#[test]
fn clear_includes_unindexed_legacy_secrets_and_preserves_policies_and_other_services() {
  let policy = (
    format!(
      "{}.{}",
      super::super::SAVE_POLICY_SERVICE_PREFIX,
      "a".repeat(64)
    ),
    "policy".to_owned(),
  );
  let unrelated = ("another-app.password".to_owned(), "alice".to_owned());
  let similar = (format!("{}.other", identity::SERVICE), "b".repeat(64));
  let mut store = FakeStore::new(vec![
    record(&ssh_service('a'), &"b".repeat(64)),
    record(&ssh_service('c'), &"d".repeat(64)),
    record(identity::SERVICE, &"e".repeat(64)),
    record(&policy.0, &policy.1),
    record(&unrelated.0, &unrelated.1),
    record(&similar.0, &similar.1),
  ]);
  assert_eq!(
    run_with(&mut store).unwrap(),
    ClearCounts {
      credential_count: 2,
      identity_count: 1,
    }
  );
  assert_eq!(store.items, BTreeSet::from([policy, unrelated, similar]));
  assert_eq!(store.deletes.len(), 3);
  assert_eq!(store.metadata, BTreeSet::from(["import-complete".into()]));
  assert!(store.reset);
}

#[test]
fn empty_owned_inventory_commits_a_complete_empty_index() {
  let mut store = FakeStore::new(vec![record("unrelated", "account")]);
  assert_eq!(run_with(&mut store).unwrap(), ClearCounts::default());
  assert_eq!(store.items.len(), 1);
  assert_eq!(store.deletes, Vec::<(String, String)>::new());
  assert!(store.reset);
}

#[test]
fn truncated_scan_is_rejected_before_any_mutation() {
  let mut store = FakeStore::new(
    (0..=MAX_ITEMS)
      .map(|_| record(&ssh_service('a'), &"b".repeat(64)))
      .collect(),
  );
  assert!(run_with(&mut store).is_err());
  assert!(!store.began);
  assert_eq!(store.deletes, Vec::<(String, String)>::new());
  assert!(!store.reset);
}

#[test]
fn malformed_owned_selector_is_rejected_after_a_valid_item_without_deleting_it() {
  for malformed in [
    record(&format!("{SERVICE_PREFIX}not-a-scope"), &"b".repeat(64)),
    record(&ssh_service('a'), "not-an-account"),
    record(identity::SERVICE, "not-an-identity"),
    Record {
      attributes: HashMap::from([("svce".into(), identity::SERVICE.into())]),
      created_at_ms: None,
      updated_at_ms: None,
      secret: None,
    },
    Record {
      attributes: HashMap::new(),
      created_at_ms: None,
      updated_at_ms: None,
      secret: None,
    },
  ] {
    let mut store = FakeStore::new(vec![record(&ssh_service('c'), &"d".repeat(64)), malformed]);
    let initial = store.items.clone();
    assert!(run_with(&mut store).is_err());
    assert_eq!(store.items, initial);
    assert!(!store.began);
    assert_eq!(store.deletes, Vec::<(String, String)>::new());
  }
}

#[test]
fn unexpected_secret_payload_or_duplicate_selector_is_rejected_without_mutation() {
  let mut unexpected = record(&ssh_service('a'), &"b".repeat(64));
  unexpected.secret = Some(zeroize::Zeroizing::new(b"synthetic-secret".to_vec()));
  for records in [
    vec![unexpected],
    vec![
      record(&ssh_service('a'), &"b".repeat(64)),
      record(&ssh_service('a'), &"b".repeat(64)),
    ],
  ] {
    let mut store = FakeStore::new(records);
    assert!(run_with(&mut store).is_err());
    assert!(!store.began);
    assert_eq!(store.deletes, Vec::<(String, String)>::new());
  }
}

#[test]
fn scan_failure_does_not_touch_any_entry_or_metadata() {
  let mut store = FakeStore::new(vec![record(&ssh_service('a'), &"b".repeat(64))]);
  store.failure = Some(Failure::Scan);
  assert!(run_with(&mut store).is_err());
  assert_eq!(store.items.len(), 1);
  assert_eq!(store.metadata, BTreeSet::from(["old indexed item".into()]));
  assert!(!store.began);
}

#[test]
fn partial_deletion_failure_retains_pending_state_and_never_claims_cleared() {
  let mut store = FakeStore::new(vec![
    record(&ssh_service('a'), &"b".repeat(64)),
    record(identity::SERVICE, &"c".repeat(64)),
  ]);
  store.failure = Some(Failure::Delete(2));
  assert!(run_with(&mut store).is_err());
  assert_eq!(store.items.len(), 1);
  assert!(store.metadata.contains("pending"));
  assert!(!store.reset);
}

#[test]
fn failed_index_commit_does_not_claim_success_after_secret_deletion() {
  let mut store = FakeStore::new(vec![record(identity::SERVICE, &"a".repeat(64))]);
  store.failure = Some(Failure::Reset);
  assert!(run_with(&mut store).is_err());
  assert!(store.items.is_empty());
  assert!(store.metadata.contains("pending"));
  assert!(!store.reset);
}
