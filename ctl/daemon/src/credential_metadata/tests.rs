use super::*;

fn target() -> SshTarget {
  serde_json::from_value(serde_json::json!({
    "destination": "alice@fixture.example",
    "hostname": null,
    "user": null,
    "port": null,
    "identity_file": null,
    "gateways": [],
  }))
  .unwrap()
}

fn attributes() -> HashMap<String, String> {
  HashMap::from([
    ("svce".into(), format!("{SERVICE_PREFIX}{}", "a".repeat(64))),
    ("acct".into(), "b".repeat(64)),
  ])
}

fn inventory_from_attributes(
  attributes: impl IntoIterator<Item = Option<HashMap<String, String>>>,
) -> Inventory {
  super::inventory_from_attributes(attributes.into_iter().map(|values| {
    values.map(|values| Attributes {
      values,
      created_at_ms: Some(1_600_000_000_000),
      updated_at_ms: Some(1_600_000_060_000),
    })
  }))
}

#[test]
fn legacy_and_orphaned_credentials_remain_visible_without_guessing_their_type() {
  let inventory = inventory_from_attributes([Some(attributes())]);
  assert!(inventory.complete);
  assert!(inventory.warning.is_none());
  let credential = &inventory.credentials[0];
  assert_eq!(credential.kind, CredentialKind::SshCredential);
  assert_eq!(credential.name, "Saved SSH credential · bbbbbbbb");
  assert_eq!(credential.scope_id, "a".repeat(64));
  assert_eq!(credential.target, None);
  assert_eq!(credential.account, None);
  assert_eq!(credential.key_name, None);
  assert_eq!(credential.created_at_ms, Some(1_600_000_000_000));
  assert_eq!(credential.updated_at_ms, Some(1_600_000_060_000));
}

#[test]
fn inventory_excludes_other_services_and_save_preferences() {
  let mut other = attributes();
  other.insert("svce".into(), "unrelated.application".into());
  let mut policy = attributes();
  policy.insert(
    "svce".into(),
    format!("dev.tokn-ai.ctl.ctld.ssh-save-policy.{}", "a".repeat(64)),
  );
  policy.insert("acct".into(), "policy".into());
  let inventory = inventory_from_attributes([Some(other), Some(policy), Some(attributes())]);
  assert!(inventory.complete);
  assert_eq!(inventory.credentials.len(), 1);
}

#[test]
fn metadata_is_projected_without_other_attributes_or_secret_values() {
  let mut attributes = attributes();
  let metadata = Metadata::from_prompt(&target(), "alice@fixture.example's password:");
  attributes.insert("icmt".into(), serde_json::to_string(&metadata).unwrap());
  attributes.insert("v_Data".into(), "NEVER_SERIALIZE_SECRET".into());
  attributes.insert("labl".into(), "NEVER_SERIALIZE_RAW_LABEL".into());
  attributes.insert(
    "unexpected".into(),
    "NEVER_SERIALIZE_UNKNOWN_ATTRIBUTE".into(),
  );
  let inventory = inventory_from_attributes([Some(attributes)]);
  let credential = &inventory.credentials[0];
  assert_eq!(credential.kind, CredentialKind::SshPassword);
  assert_eq!(credential.target.as_deref(), Some("alice@fixture.example"));
  assert_eq!(credential.account.as_deref(), Some("alice"));
  assert!(credential.created_at_ms.is_some());
  assert!(credential.updated_at_ms.is_some());
  let wire = serde_json::to_string(&inventory).unwrap();
  assert!(!wire.contains("NEVER_SERIALIZE"));
  assert!(!wire.contains("password:"));
}

#[test]
fn key_passphrase_metadata_keeps_only_the_basename() {
  let metadata = Metadata::from_prompt(
    &target(),
    "Enter passphrase for key '/private/keys/work_key': ",
  );
  assert_eq!(metadata.kind, CredentialKind::SshKeyPassphrase);
  assert_eq!(metadata.key_name.as_deref(), Some("work_key"));
  assert_eq!(metadata.name(), "SSH key passphrase · work_key");
  assert!(
    !serde_json::to_string(&metadata)
      .unwrap()
      .contains("/private/keys")
  );
}

#[test]
fn prompt_text_is_not_saved_or_used_as_a_label() {
  let prompt = "Server challenge NEVER_SERIALIZE_CHALLENGE password:";
  let metadata = Metadata::from_prompt(&target(), prompt);
  assert_eq!(metadata.kind, CredentialKind::SshPassword);
  assert!(
    !serde_json::to_string(&metadata)
      .unwrap()
      .contains("NEVER_SERIALIZE")
  );
  let metadata = Metadata::from_prompt(
    &target(),
    "Enter passphrase for key NEVER_SERIALIZE_CHALLENGE",
  );
  assert_eq!(metadata.key_name, None);
}

#[test]
fn metadata_is_bounded_and_strips_control_characters() {
  let mut target = target();
  target.destination = format!("\n{}\u{001b}", "x".repeat(1024));
  let metadata = Metadata::from_prompt(&target, "password:");
  assert_eq!(metadata.target, "x".repeat(MAX_TEXT_CHARACTERS));
  assert!(!metadata.name().chars().any(char::is_control));
}

#[test]
fn malformed_or_future_metadata_preserves_the_legacy_item() {
  for comment in ["not json".into(), "x".repeat(MAX_METADATA_BYTES + 1)] {
    let mut attributes = attributes();
    attributes.insert("icmt".into(), comment);
    let inventory = inventory_from_attributes([Some(attributes)]);
    assert!(inventory.complete);
    assert_eq!(inventory.credentials[0].kind, CredentialKind::SshCredential);
  }
  let mut metadata = Metadata::from_prompt(&target(), "password:");
  metadata.version = METADATA_VERSION + 1;
  assert!(Metadata::from_json(&serde_json::to_string(&metadata).unwrap()).is_none());
}

#[test]
fn malformed_owned_identifiers_mark_inventory_incomplete() {
  let mut malformed = attributes();
  malformed.insert("acct".into(), "policy".into());
  let inventory = inventory_from_attributes([Some(malformed), Some(attributes())]);
  assert_eq!(inventory.credentials.len(), 1);
  assert!(!inventory.complete);
  assert!(inventory.warning.is_some());
  assert!(!inventory_from_attributes([None]).complete);
}

#[test]
fn search_limit_is_reported_without_claiming_an_empty_complete_inventory() {
  let inventory = inventory_from_attributes(
    std::iter::repeat_with(|| Some(attributes())).take(MAX_SEARCH_ITEMS + 1),
  );
  assert_eq!(inventory.credentials.len(), MAX_SEARCH_ITEMS);
  assert!(!inventory.complete);
  assert_eq!(inventory.warning.as_deref(), Some(TRUNCATED_WARNING));
}

#[test]
fn identifiers_cannot_delete_another_service_or_a_save_preference() {
  let valid = format!("{}:{}", "a".repeat(64), "b".repeat(64));
  assert!(item_identity(&valid).is_some());
  for invalid in [
    String::new(),
    format!("{}:policy", "a".repeat(64)),
    format!("{}:{}", "A".repeat(64), "b".repeat(64)),
    format!("{}:{}:extra", "a".repeat(64), "b".repeat(64)),
    format!("{SERVICE_PREFIX}{valid}"),
    "../other-service".into(),
  ] {
    assert!(item_identity(&invalid).is_none());
  }
}

#[test]
fn saved_scope_identity_remains_compatible_with_the_original_layout() {
  use sha2::{Digest, Sha256};
  use std::fmt::Write as _;

  let target = target();
  let bytes = serde_json::to_vec(&(&target.destination, &target.gateways)).unwrap();
  let original = Sha256::digest(bytes)
    .iter()
    .fold(String::new(), |mut output, byte| {
      write!(output, "{byte:02x}").unwrap();
      output
    });
  assert_eq!(ctl_ipc::credentials::scope_id(&target), original);
  let mut changed = target;
  changed.port = Some(2222);
  changed.user = Some("alternate".into());
  assert_eq!(ctl_ipc::credentials::scope_id(&changed), original);
}
