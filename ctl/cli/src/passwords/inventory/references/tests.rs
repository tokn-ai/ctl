use super::*;
use crate::passwords::inventory::Removal;
use crate::passwords::inventory::fixtures;
use ctl_ipc::credentials::PasswordState;

fn password(account: &str, name: &str) -> Entry {
  Entry::discovered(fixtures::password(account, name))
}

fn identity() -> Entry {
  Entry::discovered(fixtures::identity(&"b".repeat(64), PasswordState::Saved))
}

#[test]
fn displayed_references_identify_the_original_stored_items() {
  let entries = vec![
    password("first", "Work"),
    password("second", "Work"),
    identity(),
  ];
  let first = short_id(&entries[0], &entries);
  let second = short_id(&entries[1], &entries);
  let key = short_id(&entries[2], &entries);
  assert!(first.starts_with("p-"));
  assert!(key.starts_with("k-"));
  assert_eq!(first.len(), PREFIX_LENGTH + MIN_DIGEST_LENGTH);
  assert_ne!(first, second);
  for (entry, alias) in entries.iter().zip([first, second, key]) {
    let resolved = select(&entries, &alias).unwrap().unwrap();
    assert_eq!(resolved.id, entry.id);
    assert_eq!(resolved.removal(), entry.removal());
  }
  assert!(matches!(entries[0].removal(), Removal::Credential(_)));
  assert!(matches!(entries[2].removal(), Removal::Identity(_)));
}

#[test]
fn references_remain_bound_when_order_and_metadata_change() {
  let first = password("first", "Original name");
  let alias = short_id(&first, std::slice::from_ref(&first));
  let mut renamed = first.clone();
  renamed.name = "New name".into();
  renamed.target = Some("alice@new-address.test".into());
  let entries = vec![password("unrelated", "Other"), renamed];
  let resolved = select(&entries, &alias).unwrap().unwrap();
  assert_eq!(resolved.id, first.id);
  assert_eq!(resolved.removal(), first.removal());
  assert_eq!(short_id(resolved, &entries), alias);
}

#[test]
fn shared_digest_prefixes_extend_without_selecting_arbitrarily() {
  let mut first = password("first", "First");
  let mut second = password("second", "Second");
  first.reference = format!("p-{}a{}", "1".repeat(12), "0".repeat(51));
  second.reference = format!("p-{}b{}", "1".repeat(12), "0".repeat(51));
  let entries = vec![first, second];
  let short = format!("p-{}", "1".repeat(12));
  assert!(
    select(&entries, &short)
      .unwrap()
      .unwrap_err()
      .contains("ambiguous")
  );
  for entry in &entries {
    let alias = short_id(entry, &entries);
    assert_eq!(alias.len(), PREFIX_LENGTH + MIN_DIGEST_LENGTH + 1);
    assert_eq!(select(&entries, &alias).unwrap().unwrap().id, entry.id);
  }
}

#[test]
fn references_extend_past_names_and_conflicting_aliases_are_rejected() {
  let first = password("first", "First");
  let alias = short_id(&first, std::slice::from_ref(&first));
  let second = password("second", &alias);
  let entries = vec![first, second];
  assert!(
    select(&entries, &alias)
      .unwrap()
      .unwrap_err()
      .contains("ambiguous")
  );
  let extended = short_id(&entries[0], &entries);
  assert_eq!(extended.len(), alias.len() + 1);
  assert_eq!(
    select(&entries, &extended).unwrap().unwrap().id,
    entries[0].id
  );
}

#[test]
fn removed_reference_never_retargets_an_unrelated_name() {
  let removed = password("removed", "Removed");
  let alias = short_id(&removed, std::slice::from_ref(&removed));
  let entries = vec![password("unrelated", &alias)];
  let error = select(&entries, &alias).unwrap().unwrap_err();
  assert!(error.contains("No saved password"));
}

#[test]
fn full_digest_collisions_require_authoritative_ids() {
  let first = password("first", "First");
  let mut second = password("second", "Second");
  second.reference.clone_from(&first.reference);
  let entries = vec![first, second];
  for entry in &entries {
    assert_eq!(short_id(entry, &entries), entry.id);
    assert!(
      select(&entries, &entry.reference)
        .unwrap()
        .unwrap_err()
        .contains("ambiguous")
    );
  }
}

#[test]
fn names_conflicting_at_every_digest_length_require_authoritative_id() {
  let first = password("first", "First");
  let mut entries = vec![first];
  for length in MIN_DIGEST_LENGTH..=DIGEST_LENGTH {
    let alias = entries[0].reference[..PREFIX_LENGTH + length].to_owned();
    entries.push(password(&format!("other-{length}"), &alias));
  }
  assert_eq!(short_id(&entries[0], &entries), entries[0].id);
}

#[test]
fn alias_syntax_is_reserved_only_for_supported_prefixes_and_lengths() {
  let entry = password("first", "First");
  let entries = vec![entry];
  for selector in [
    "Work",
    "p-123456789ab",
    "p-123456789abcg",
    "p-123456789ABC",
    "x-123456789abc",
    "password:original",
  ] {
    assert!(select(&entries, selector).is_none());
  }
  assert!(select(&entries, &format!("p-{}", "1".repeat(65))).is_none());
  assert!(select(&entries, "p-123456789abc").unwrap().is_err());
  assert!(select(&entries, "k-123456789abc").unwrap().is_err());
}

#[test]
fn matching_own_name_is_not_ambiguous() {
  let mut entry = password("first", "First");
  entry.name = short_id(&entry, std::slice::from_ref(&entry));
  let alias = entry.name.clone();
  let entries = vec![entry];
  assert_eq!(select(&entries, &alias).unwrap().unwrap().id, entries[0].id);
}
