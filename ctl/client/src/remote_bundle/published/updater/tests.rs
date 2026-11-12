use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-cache-test-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path).unwrap();
    Self(path)
  }

  fn cache(&self) -> PathBuf {
    self.0.join(".tokn/ctl/cache/updaters")
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn fixture() -> VerifiedUpdater {
  let bytes = b"updater fixture".to_vec();
  let target = "x86_64-unknown-linux-musl";
  let bundle_id = "0.1.0-nightly.20261110+gaaaaaaaaaaaa";
  VerifiedUpdater {
    info: UpdaterInfo {
      schema_version: 1,
      app_version: "0.1.0".into(),
      bundle_id: bundle_id.into(),
      git_revision: "a".repeat(40),
      target_triple: target.into(),
      binary: format!("ctlup-{bundle_id}-{target}"),
      sha256: format!("{:x}", Sha256::digest(&bytes)),
      size: bytes.len() as u64,
    },
    bytes,
  }
}

#[test]
fn updater_metadata_rejects_other_targets_source_identity_and_changed_bytes() {
  let updater = fixture();
  let value = serde_json::to_value(&updater.info).unwrap();
  for (field, replacement) in [
    (
      "target_triple",
      serde_json::json!("aarch64-unknown-linux-musl"),
    ),
    ("bundle_id", serde_json::json!("another-release")),
    ("binary", serde_json::json!("../ctlup")),
    ("size", serde_json::json!(0)),
  ] {
    let mut changed = value.clone();
    changed[field] = replacement;
    assert!(
      UpdaterInfo::parse(
        &serde_json::to_vec(&changed).unwrap(),
        &updater.info.target_triple
      )
      .is_err()
    );
  }
  assert!(updater.info.verify(b"changed bytes".to_vec()).is_err());
  let mut wrong_digest = updater.info.clone();
  wrong_digest.sha256 = "b".repeat(64);
  assert!(wrong_digest.verify(updater.bytes).is_err());
}

#[test]
fn cache_repairs_corrupt_owned_content_and_reuses_complete_concurrent_writes() {
  let home = Home::new();
  let updater = fixture();
  let root = home.cache();
  cache_verified(&root, &updater).unwrap();
  let binary = root.join(&updater.info.sha256).join("ctlup");
  std::fs::write(&binary, b"truncated").unwrap();
  assert!(read_cached_updater(&root, &updater.info).unwrap().is_none());
  cache_verified(&root, &updater).unwrap();
  assert_eq!(
    read_cached_updater(&root, &updater.info)
      .unwrap()
      .unwrap()
      .bytes,
    updater.bytes
  );
  assert_eq!(
    std::fs::metadata(&binary).unwrap().permissions().mode() & 0o777,
    0o700
  );
  let workers: Vec<_> = (0..2)
    .map(|_| {
      let root = root.clone();
      let updater = updater.clone();
      std::thread::spawn(move || cache_verified(&root, &updater).unwrap())
    })
    .collect();
  for worker in workers {
    worker.join().unwrap();
  }
  assert!(std::fs::read_dir(root).unwrap().all(|entry| {
    !entry
      .unwrap()
      .file_name()
      .to_string_lossy()
      .starts_with('.')
  }));
}

#[test]
fn cache_rejects_symlink_parents_and_content_directories_without_touching_targets() {
  let home = Home::new();
  let updater = fixture();
  let root = home.cache();
  crate::installation::safe_directory(&root).unwrap();
  let outside = home.0.join("outside");
  private_fs::create_directory(&outside).unwrap();
  std::fs::write(outside.join("ctlup"), b"untouched").unwrap();
  let blob = root.join(&updater.info.sha256);
  std::os::unix::fs::symlink(&outside, &blob).unwrap();
  assert!(read_cached_updater(&root, &updater.info).is_err());
  assert!(cache_verified(&root, &updater).is_err());
  std::fs::remove_file(blob).unwrap();
  std::fs::remove_dir_all(&root).unwrap();
  std::os::unix::fs::symlink(&outside, &root).unwrap();
  assert!(read_cached_updater(&root, &updater.info).is_err());
  assert!(cache_verified(&root, &updater).is_err());
  assert_eq!(std::fs::read(outside.join("ctlup")).unwrap(), b"untouched");
}

#[test]
fn updater_cache_budget_evicts_old_blobs_and_their_records_but_keeps_newer_content() {
  let home = Home::new();
  let old = fixture();
  let mut new = fixture();
  new.bytes = b"new updater fixture".to_vec();
  new.info.sha256 = format!("{:x}", Sha256::digest(&new.bytes));
  new.info.size = new.bytes.len() as u64;
  let root = home.cache();
  cache_verified(&root, &old).unwrap();
  cache_verified(&root, &new).unwrap();
  for (updater, seconds) in [(&old, 1), (&new, 2)] {
    let file = std::fs::File::open(root.join(&updater.info.sha256).join("ctlup")).unwrap();
    file
      .set_times(
        std::fs::FileTimes::new()
          .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
      )
      .unwrap();
  }
  let records = root.join("records");
  crate::installation::safe_directory(&records).unwrap();
  for (updater, id) in [(&old, 'a'), (&new, 'b')] {
    let path = records.join(format!("{}.json", id.to_string().repeat(64)));
    std::fs::write(&path, serde_json::to_vec(&updater.info).unwrap()).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
  }
  assert_eq!(
    prune_updater_cache(&root, new.info.size).unwrap(),
    old.info.size
  );
  assert!(read_cached_updater(&root, &old.info).unwrap().is_none());
  assert_eq!(
    read_cached_updater(&root, &new.info)
      .unwrap()
      .unwrap()
      .bytes,
    new.bytes
  );
  assert!(!records.join(format!("{}.json", "a".repeat(64))).exists());
  assert!(records.join(format!("{}.json", "b".repeat(64))).exists());
  assert_eq!(prune_updater_cache(&root, 0).unwrap(), new.info.size);
  assert_eq!(std::fs::read_dir(records).unwrap().count(), 0);
}

#[test]
fn invalid_cache_records_stop_cleanup_before_removing_valid_blobs() {
  let home = Home::new();
  let updater = fixture();
  let root = home.cache();
  cache_verified(&root, &updater).unwrap();
  let records = root.join("records");
  crate::installation::safe_directory(&records).unwrap();
  let path = records.join(format!("{}.json", "a".repeat(64)));
  std::fs::write(&path, b"{}").unwrap();
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
  assert!(prune_updater_cache(&root, 0).is_err());
  assert_eq!(
    read_cached_updater(&root, &updater.info)
      .unwrap()
      .unwrap()
      .bytes,
    updater.bytes
  );
}
