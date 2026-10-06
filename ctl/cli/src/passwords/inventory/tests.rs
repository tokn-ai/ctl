use super::*;
use credentials::{CredentialKind, PasswordState};
use fixtures::{discovery, identity, password};
use identities::FileState;

#[test]
fn lists_stored_passwords_and_passphrases_with_authoritative_ids() {
  let password = password(&"b".repeat(64), "Work password");
  let credential_id = password.id.clone();
  let identity_id = "c".repeat(64);
  let mut changed = identity(&"d".repeat(64), PasswordState::FileChanged);
  changed.path = Some("/home/alice/.ssh/removed".into());
  changed.display_path = Some("~/.ssh/removed".into());
  changed.name = "~/.ssh/removed".into();
  changed.file_state = Some(FileState::Missing);
  changed.file_version = None;
  let snapshot = build(discovery(vec![
    password,
    identity(&identity_id, PasswordState::Saved),
    changed,
  ]));
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
  let snapshot = build(discovery(vec![legacy]));
  let entry = &snapshot.entries[0];
  assert_eq!(entry.kind_label(), "Legacy passphrase");
  assert!(matches!(entry.removal(), Removal::Credential(_)));
  assert_eq!(serde_json::to_value(entry).unwrap()["source"], "credential");
}

#[test]
fn complete_discovery_is_independent_of_missing_or_malformed_metadata() {
  let mut unknown = identity("malformed-stored-id", PasswordState::Unknown);
  unknown.name = "Unknown saved key passphrase".into();
  unknown.path = None;
  unknown.display_path = None;
  unknown.file_state = None;
  unknown.detail = Some("Saved key metadata could not be decoded.".into());
  let snapshot = build(discovery(vec![unknown]));
  assert!(snapshot.complete);
  assert_eq!(snapshot.entries.len(), 1);
  let entry = &snapshot.entries[0];
  assert_eq!(entry.state, State::Unknown);
  assert_eq!(entry.id, "identity:malformed-stored-id");
  assert_eq!(entry.removal(), Removal::Identity("malformed-stored-id"));
  assert_eq!(snapshot.select(&snapshot.short_id(entry)).unwrap(), entry);
  assert!(snapshot.render_list().contains("unknown"));
  assert!(snapshot.render_show(entry).contains("could not be decoded"));
  assert_eq!(snapshot.warnings, Vec::<String>::new());
  let value = serde_json::to_value(&snapshot).unwrap();
  assert_eq!(value["complete"], true);
  assert_eq!(value["entries"][0]["state"], "unknown");
  assert!(value.get("metadata_import_required").is_none());
}

#[test]
fn unknown_password_metadata_retains_its_raw_id_for_helper_validation() {
  let mut unknown = password("invalid-account-id", "Unknown SSH credential");
  unknown.state = PasswordState::Unknown;
  unknown.account = None;
  unknown.target = None;
  unknown.detail = Some("Stored credential account metadata is malformed.".into());
  let raw_id = unknown.id.clone();
  let snapshot = build(discovery(vec![unknown]));
  let entry = &snapshot.entries[0];
  assert_eq!(entry.state, State::Unknown);
  assert_eq!(entry.removal(), Removal::Credential(&raw_id));
  assert!(snapshot.render_show(entry).contains("malformed"));
  assert!(entry.scope_id.is_none());
  assert_eq!(snapshot.choices().len(), 1);
}

#[test]
fn retains_specific_discovery_warnings_without_duplicate_or_generic_messages() {
  let mut inventory = discovery(Vec::new());
  inventory.complete = false;
  inventory.warnings = vec![
    "Keychain access was denied while scanning saved passwords.".into(),
    "Keychain access was denied while scanning saved passwords.".into(),
    String::new(),
    "  ".into(),
  ];
  let snapshot = build(inventory);
  assert!(!snapshot.complete);
  assert_eq!(snapshot.warnings.len(), 1);
  assert!(snapshot.warnings[0].contains("access was denied"));
  assert!(snapshot.render_list().contains("inventory is incomplete"));
}

#[test]
fn incomplete_discovery_with_no_reason_has_one_fallback_warning() {
  let mut inventory = discovery(Vec::new());
  inventory.complete = false;
  inventory.warnings = vec!["  ".into()];
  let snapshot = build(inventory);
  assert!(!snapshot.complete);
  assert_eq!(snapshot.warnings.len(), 1);
  assert!(snapshot.warnings[0].contains("discovery did not finish"));
}

#[test]
fn selects_unique_names_and_id_prefixes_but_rejects_ambiguous_or_blank_input() {
  let first = password(&"b".repeat(64), "Repeated name");
  let second = password(&"c".repeat(64), "Repeated name");
  let first_id = format!("password:{}", first.id);
  let snapshot = build(discovery(vec![first, second]));
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
  let first_id = format!("password:{}", first.id);
  let second = password(&"c".repeat(64), &first_id);
  let snapshot = build(discovery(vec![first, second]));
  assert_eq!(snapshot.select(&first_id).unwrap().name, "First");
}

#[test]
fn serialization_exposes_only_public_metadata_in_snake_case() {
  let snapshot = build(discovery(vec![
    password(&"b".repeat(64), "Work password"),
    identity(&"c".repeat(64), PasswordState::Saved),
  ]));
  let value = serde_json::to_value(&snapshot).unwrap();
  let password = value["entries"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["source"] == "credential")
    .unwrap();
  assert_eq!(password["scope_id"], "a".repeat(64));
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
  let snapshot = build(discovery(vec![password(
    &"b".repeat(64),
    "Bad\nname\u{1b}[2J\u{202e}",
  )]));
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
  let snapshot = build(discovery(Vec::new()));
  assert_eq!(snapshot.render_list(), "No saved passwords.");
  assert_eq!(snapshot.warnings, Vec::<String>::new());
}
