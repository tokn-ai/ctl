use super::*;

fn snapshot() -> Snapshot {
  inventory::build(inventory::fixtures::discovery(vec![
    inventory::fixtures::password(&"b".repeat(64), "SSH password · work"),
  ]))
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
