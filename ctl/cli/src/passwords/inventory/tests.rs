use super::*;
use credentials::{CredentialKind, StoredCredential};
use identities::{FileState, IdentityFile, PassphraseState};

fn password(account: &str, name: &str) -> StoredCredential {
  StoredCredential {
    credential_id: format!("{}:{}", "a".repeat(64), account),
    scope_id: "a".repeat(64),
    name: name.into(),
    kind: CredentialKind::SshPassword,
    target: Some("alice@example.test".into()),
    account: Some("alice".into()),
    key_name: None,
    created_at_ms: Some(1),
    updated_at_ms: Some(2),
  }
}

fn identity(id: &str, passphrase_state: PassphraseState) -> IdentityFile {
  IdentityFile {
    identity_id: id.into(),
    path: "/home/alice/.ssh/id_ed25519".into(),
    display_path: "~/.ssh/id_ed25519".into(),
    file_version: Some("version".into()),
    key_type: Some("ssh-ed25519".into()),
    fingerprint: Some("SHA256:public-fingerprint".into()),
    encrypted: Some(true),
    file_state: FileState::Ready,
    passphrase_state,
    detail: None,
  }
}

fn inventories(
  credentials: Vec<StoredCredential>,
  identity_files: Vec<IdentityFile>,
) -> (credentials::Inventory, identities::Inventory) {
  (
    credentials::Inventory {
      credentials,
      complete: true,
      warning: None,
      metadata_import_required: false,
    },
    identities::Inventory {
      identity_files,
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
fn lists_stored_passwords_and_passphrases_with_authoritative_ids() {
  let password = password(&"b".repeat(64), "Work password");
  let credential_id = password.credential_id.clone();
  let identity_id = "c".repeat(64);
  let mut changed = identity(&"d".repeat(64), PassphraseState::FileChanged);
  changed.path = "/home/alice/.ssh/removed".into();
  changed.display_path = "~/.ssh/removed".into();
  changed.file_state = FileState::Missing;
  changed.file_version = None;
  let (credentials, identities) = inventories(
    vec![password],
    vec![
      identity(&identity_id, PassphraseState::Saved),
      changed,
      identity(&"e".repeat(64), PassphraseState::NotSaved),
      identity(&"f".repeat(64), PassphraseState::NotRequired),
      identity(&"0".repeat(64), PassphraseState::Unknown),
    ],
  );
  let snapshot = build(credentials, identities);
  assert!(snapshot.complete);
  assert_eq!(snapshot.entries.len(), 3);
  let stored_password = snapshot.select("Work password").unwrap();
  assert_eq!(stored_password.id, format!("password:{credential_id}"));
  assert_eq!(
    stored_password.removal(),
    Removal::Credential(&credential_id)
  );
  let stored_identity = snapshot.select("~/.ssh/id_ed25519").unwrap();
  assert_eq!(stored_identity.id, format!("identity:{identity_id}"));
  assert_eq!(stored_identity.removal(), Removal::Identity(&identity_id));
  assert_eq!(
    snapshot.select("~/.ssh/removed").unwrap().state,
    State::FileChanged
  );
}

#[test]
fn legacy_credentials_are_distinguished_from_verified_identity_passphrases() {
  let mut legacy = password(&"b".repeat(64), "Legacy key passphrase");
  legacy.kind = CredentialKind::SshKeyPassphrase;
  let (credentials, identities) = inventories(vec![legacy], Vec::new());
  let snapshot = build(credentials, identities);
  let entry = &snapshot.entries[0];
  assert_eq!(entry.kind_label(), "Legacy passphrase");
  assert!(matches!(entry.removal(), Removal::Credential(_)));
  assert_eq!(serde_json::to_value(entry).unwrap()["source"], "credential");
}

#[test]
fn legacy_metadata_requirement_prevents_complete_empty_inventory() {
  let (mut credentials, identities) = inventories(Vec::new(), Vec::new());
  credentials.metadata_import_required = true;
  let snapshot = build(credentials, identities);
  assert!(!snapshot.complete);
  assert!(snapshot.metadata_import_required);
  assert!(snapshot.render_list().contains("inventory is incomplete"));
  assert!(
    snapshot
      .warnings
      .iter()
      .any(|warning| warning.contains("Older saved credentials"))
  );
  let value = serde_json::to_value(&snapshot).unwrap();
  assert_eq!(value["complete"], false);
  assert_eq!(value["metadata_import_required"], true);
}

#[test]
fn retains_source_warnings_and_keychain_unavailability() {
  let (mut credentials, mut identities) = inventories(Vec::new(), Vec::new());
  credentials.complete = false;
  credentials.warning = Some("Password metadata was unavailable.".into());
  identities.keychain_available = false;
  identities.keychain_error = Some("keychain_unavailable".into());
  let snapshot = build(credentials, identities);
  assert!(!snapshot.complete);
  assert_eq!(snapshot.warnings.len(), 2);
  assert!(
    snapshot
      .warnings
      .iter()
      .any(|warning| warning.contains("Keychain access"))
  );
  assert!(
    snapshot
      .warnings
      .iter()
      .any(|warning| warning.contains("Password metadata"))
  );
}

#[test]
fn selects_unique_names_and_id_prefixes_but_rejects_ambiguous_or_blank_input() {
  let first = password(&"b".repeat(64), "Repeated name");
  let second = password(&"c".repeat(64), "Repeated name");
  let first_id = format!("password:{}", first.credential_id);
  let (credentials, identities) = inventories(vec![first, second], Vec::new());
  let snapshot = build(credentials, identities);
  assert_eq!(snapshot.select(&first_id).unwrap().id, first_id);
  assert_eq!(
    snapshot.select(&first_id[..first_id.len() - 4]).unwrap().id,
    first_id
  );
  assert!(
    snapshot
      .select("Repeated name")
      .unwrap_err()
      .contains("ambiguous")
  );
  assert!(
    snapshot
      .select("password:")
      .unwrap_err()
      .contains("ambiguous")
  );
  assert!(snapshot.select("").is_err());
  assert!(snapshot.select("  ").is_err());
  assert!(
    snapshot
      .select("missing")
      .unwrap_err()
      .contains("No saved password")
  );
}

#[test]
fn exact_authoritative_id_wins_over_another_entry_name() {
  let first = password(&"b".repeat(64), "First");
  let first_id = format!("password:{}", first.credential_id);
  let second = password(&"c".repeat(64), &first_id);
  let (credentials, identities) = inventories(vec![first, second], Vec::new());
  let snapshot = build(credentials, identities);
  assert_eq!(snapshot.select(&first_id).unwrap().name, "First");
}

#[test]
fn serialization_exposes_only_public_metadata_in_snake_case() {
  let (credentials, identities) = inventories(
    vec![password(&"b".repeat(64), "Work password")],
    vec![identity(&"c".repeat(64), PassphraseState::Saved)],
  );
  let snapshot = build(credentials, identities);
  let value = serde_json::to_value(&snapshot).unwrap();
  for entry in value["entries"].as_array().unwrap() {
    let fields = entry.as_object().unwrap();
    for field in fields.keys() {
      assert!(!field.chars().any(char::is_uppercase));
      assert!(
        !["password", "passphrase", "secret", "value", "stored_id"].contains(&field.as_str())
      );
    }
    assert!(fields.contains_key("id"));
    assert!(fields.contains_key("kind"));
    assert!(fields.contains_key("state"));
  }
  let identity = value["entries"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["source"] == "identity")
    .unwrap();
  assert_eq!(identity["fingerprint"], "SHA256:public-fingerprint");
  assert_eq!(identity["file_state"], "ready");
}

#[test]
fn human_output_and_selector_errors_escape_terminal_control_sequences() {
  let (credentials, identities) = inventories(
    vec![password(&"b".repeat(64), "Bad\nname\u{1b}[2J\u{202e}")],
    Vec::new(),
  );
  let snapshot = build(credentials, identities);
  for output in [
    snapshot.render_list(),
    snapshot.render_show(&snapshot.entries[0]),
  ] {
    assert!(!output.contains('\u{1b}'));
    assert!(!output.contains('\u{202e}'));
    assert!(output.contains("\\n"));
  }
  let error = snapshot.select("missing\u{1b}[2J").unwrap_err();
  assert!(!error.contains('\u{1b}'));
}

#[test]
fn complete_empty_inventory_is_distinct_from_an_incomplete_one() {
  let (credentials, identities) = inventories(Vec::new(), Vec::new());
  let snapshot = build(credentials, identities);
  assert_eq!(snapshot.render_list(), "No saved passwords.");
  assert_eq!(snapshot.warnings, Vec::<String>::new());
}
