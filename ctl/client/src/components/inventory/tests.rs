use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use sha2::{Digest as _, Sha256};

struct Directory(PathBuf);
impl Directory {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("ctl-bundle-inventory-{}", uuid::Uuid::new_v4())))
  }
}
impl Drop for Directory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn write_included(directory: &Path) {
  std::fs::create_dir_all(directory).unwrap();
  let mut fixture = Fixture::new("0.0.9", &"b".repeat(40));
  let targets: Vec<_> = fixture.outer["targets"]
    .as_object()
    .unwrap()
    .keys()
    .cloned()
    .collect();
  for target in targets {
    fixture.inner["target_triple"] = serde_json::json!(target);
    let archive = fixture.archive();
    fixture.outer["targets"][&target]["sha256"] =
      serde_json::json!(format!("{:x}", Sha256::digest(&archive)));
    let name = fixture.outer["targets"][&target]["archive"]
      .as_str()
      .unwrap();
    std::fs::write(directory.join(name), archive).unwrap();
  }
  std::fs::write(
    directory.join("bundle-set.json"),
    serde_json::to_vec(&fixture.outer).unwrap(),
  )
  .unwrap();
}

#[test]
fn includes_all_packaged_targets_without_importing_and_honors_filter() {
  let root = Directory::new();
  let home = root.0.join("home");
  let directories = [root.0.join("included")];
  write_included(&directories[0]);
  let listed = snapshot(&home, None, &directories);
  assert!(listed.errors.is_empty(), "{:?}", listed.errors);
  assert_eq!(listed.bundles.len(), 4);
  assert!(
    listed
      .bundles
      .iter()
      .all(|bundle| bundle.availability.included() && !bundle.availability.stored())
  );
  let target = "x86_64-unknown-linux-musl";
  let filtered = snapshot(&home, Some(target), &directories);
  assert_eq!(filtered.errors, [] as [String; 0]);
  assert_eq!(filtered.bundles.len(), 1);
  assert_eq!(filtered.bundles[0].manifest.target_triple, target);
  assert!(
    !home.exists(),
    "listing must never create or select a store"
  );
}

#[test]
fn included_choice_imports_exact_content_and_is_deduplicated_with_selection() {
  let root = Directory::new();
  let home = root.0.join("home");
  let directories = [root.0.join("included")];
  write_included(&directories[0]);
  let target = "x86_64-unknown-linux-musl";
  let mut listed = snapshot(&home, Some(target), &directories);
  let expected = listed.bundles.pop().unwrap().manifest;
  let bundle = load_selection(&home, &directories, target, &expected.bundle_id).unwrap();
  assert_eq!(bundle.manifest, expected);
  Store::new(&home).select(Purpose::Upload, &bundle).unwrap();
  let listed = snapshot(&home, None, &directories);
  assert!(listed.errors.is_empty(), "{:?}", listed.errors);
  assert_eq!(listed.bundles.len(), 4);
  let entry = listed
    .bundles
    .iter()
    .find(|entry| entry.manifest == expected)
    .unwrap();
  assert!(entry.availability.included() && entry.availability.stored() && entry.selected_upload);
  assert_eq!(
    snapshot(&home, None, &[]).bundles.len(),
    1,
    "stored foreign targets must stay visible"
  );
}

#[test]
fn damaged_included_target_reports_error_without_hiding_other_builds() {
  let root = Directory::new();
  let home = root.0.join("home");
  let directories = [root.0.join("included")];
  write_included(&directories[0]);
  let target = "x86_64-unknown-linux-musl";
  let manifest = snapshot(&home, Some(target), &directories)
    .bundles
    .pop()
    .unwrap()
    .manifest;
  let bundle = load_selection(&home, &directories, target, &manifest.bundle_id).unwrap();
  Store::new(&home).select(Purpose::Upload, &bundle).unwrap();
  std::fs::write(
    directories[0].join(format!("ctl-agent-bundle-0.0.9-{target}.tar.gz")),
    b"corrupt",
  )
  .unwrap();
  let listed = snapshot(&home, None, &directories);
  assert_eq!(listed.bundles.len(), 4);
  assert_eq!(listed.errors.len(), 1);
  assert!(listed.errors[0].contains("checksum"));
  let retained = listed
    .bundles
    .iter()
    .find(|bundle| bundle.manifest.target_triple == target)
    .unwrap();
  assert!(
    !retained.availability.included() && retained.availability.stored() && retained.selected_upload
  );
}

#[test]
fn empty_inventory_and_invalid_filters_remain_passive() {
  let home = Directory::new();
  let listed = snapshot(&home.0, None, &[]);
  assert_eq!(listed.errors, [] as [String; 0]);
  assert!(listed.bundles.is_empty());
  let invalid = snapshot(&home.0, Some("../invalid"), &[]);
  assert_ne!(invalid.errors, [] as [String; 0]);
  assert!(invalid.bundles.is_empty());
  assert!(!home.0.exists());
}
