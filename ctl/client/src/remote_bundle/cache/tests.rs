use super::*;
use crate::remote_bundle::tests::{REVISION, TARGET, VERSION, build, manifest};

struct Fixture {
  directory: PathBuf,
  cache: BundleCacheEntry,
}

impl Fixture {
  fn new() -> Self {
    let directory =
      std::env::temp_dir().join(format!("ctl-bundle-cache-test-{}", uuid::Uuid::new_v4()));
    create_private_directory(&directory).unwrap();
    let cache = BundleCacheEntry::new(&directory.join("cache"), TARGET, &build()).unwrap();
    Self { directory, cache }
  }

  fn bundle() -> VerifiedBundle {
    let bytes = serde_json::to_vec(&manifest(b"archive")).unwrap();
    let set = BundleSet::parse(&bytes, VERSION).unwrap();
    set
      .verify_archive(TARGET, b"archive".to_vec(), bytes)
      .unwrap()
  }

  fn archive_path(&self) -> PathBuf {
    self.cache.directory.join(Self::bundle().file_name)
  }

  fn assert_no_staging(&self) {
    if self.cache.revision_directory.exists() {
      assert!(
        fs::read_dir(&self.cache.revision_directory)
          .unwrap()
          .all(|entry| {
            !entry
              .unwrap()
              .file_name()
              .to_string_lossy()
              .starts_with('.')
          })
      );
    }
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.directory);
  }
}

#[test]
fn construction_and_misses_do_not_create_any_cache_paths() {
  let fixture = Fixture::new();
  assert!(!fixture.cache.root.exists());
  assert!(fixture.cache.load().unwrap().is_none());
  assert!(!fixture.cache.root.exists());
  let mut expected = build();
  expected.dirty = true;
  assert!(matches!(
    BundleCacheEntry::new(&fixture.cache.root, TARGET, &expected),
    Err(Error::Stale(_))
  ));
  expected = build();
  expected.source_revision = Some("../escape".into());
  assert!(BundleCacheEntry::new(&fixture.cache.root, TARGET, &expected).is_err());
  assert!(BundleCacheEntry::new(&fixture.cache.root, "unsupported", &build()).is_err());
  assert!(BundleCacheEntry::new(&fixture.cache.root.join("../escape"), TARGET, &build()).is_err());
}

#[test]
fn round_trip_preserves_manifest_and_reuses_the_exact_build() {
  let fixture = Fixture::new();
  let bundle = Fixture::bundle();
  fixture.cache.store(&bundle).unwrap();
  let cached = fixture.cache.load().unwrap().unwrap();
  assert_eq!(cached.archive, bundle.archive);
  assert_eq!(cached.manifest, bundle.manifest);
  assert_eq!(cached.git_revision, REVISION);
  assert_eq!(cached.file_name, bundle.file_name);
  assert_eq!(
    fixture.cache.directory,
    fixture.cache.root.join(REVISION).join(TARGET)
  );
  fixture.assert_no_staging();
}

#[test]
fn revisions_targets_and_versions_are_isolated() {
  let fixture = Fixture::new();
  fixture.cache.store(&Fixture::bundle()).unwrap();
  let mut expected = build();
  expected.source_revision = Some("a".repeat(40));
  assert!(
    BundleCacheEntry::new(&fixture.cache.root, TARGET, &expected)
      .unwrap()
      .load()
      .unwrap()
      .is_none()
  );
  assert!(
    BundleCacheEntry::new(&fixture.cache.root, "x86_64-unknown-linux-musl", &build())
      .unwrap()
      .load()
      .unwrap()
      .is_none()
  );
  expected = build();
  expected.version = "0.2.0".into();
  assert!(
    BundleCacheEntry::new(&fixture.cache.root, TARGET, &expected)
      .unwrap()
      .load()
      .unwrap()
      .is_none()
  );
}

#[test]
fn damaged_truncated_or_missing_files_are_misses_and_can_be_replaced() {
  let fixture = Fixture::new();
  for damage in 0..5 {
    fixture.cache.store(&Fixture::bundle()).unwrap();
    match damage {
      0 => fs::write(fixture.cache.directory.join(BUNDLE_SET_FILE), b"{").unwrap(),
      1 => fs::write(fixture.archive_path(), b"changed").unwrap(),
      2 => fs::remove_file(fixture.archive_path()).unwrap(),
      3 => File::options()
        .write(true)
        .open(fixture.archive_path())
        .unwrap()
        .set_len(MAX_BUNDLE_BYTES as u64 + 1)
        .unwrap(),
      _ => File::options()
        .write(true)
        .open(fixture.cache.directory.join(BUNDLE_SET_FILE))
        .unwrap()
        .set_len(MAX_BUNDLE_SET_BYTES as u64 + 1)
        .unwrap(),
    }
    assert!(fixture.cache.load().unwrap().is_none());
    fixture.cache.store(&Fixture::bundle()).unwrap();
    assert_eq!(fixture.cache.load().unwrap().unwrap().archive, b"archive");
    fixture.assert_no_staging();
  }
}

#[test]
fn public_bundle_fields_are_revalidated_before_any_write() {
  let fixture = Fixture::new();
  for field in 0..6 {
    let mut bundle = Fixture::bundle();
    match field {
      0 => bundle.app_version = "0.2.0".into(),
      1 => bundle.bundle_id = "changed".into(),
      2 => bundle.git_revision = "a".repeat(40),
      3 => bundle.file_name = "../escape".into(),
      4 => bundle.manifest = b"{}".to_vec(),
      _ => bundle.archive = b"changed".to_vec(),
    }
    assert!(fixture.cache.store(&bundle).is_err());
    assert!(!fixture.cache.root.exists());
  }
}

#[test]
fn concurrent_writers_publish_one_complete_immutable_entry() {
  use std::sync::{Arc, Barrier};
  let fixture = Fixture::new();
  let bundle = Arc::new(Fixture::bundle());
  let barrier = Arc::new(Barrier::new(4));
  let threads: Vec<_> = (0..4)
    .map(|_| {
      let cache = fixture.cache.clone();
      let bundle = Arc::clone(&bundle);
      let barrier = Arc::clone(&barrier);
      std::thread::spawn(move || {
        barrier.wait();
        match cache.store(&bundle) {
          Ok(()) => true,
          Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => false,
          Err(error) => panic!("unexpected cache publication error: {error}"),
        }
      })
    })
    .collect();
  let winners = threads
    .into_iter()
    .map(|thread| thread.join().unwrap())
    .filter(|published| *published)
    .count();
  assert!(winners >= 1);
  assert_eq!(fixture.cache.load().unwrap().unwrap().archive, b"archive");
  assert_eq!(fs::read_dir(&fixture.cache.directory).unwrap().count(), 2);
  fixture.assert_no_staging();
}

#[test]
fn a_failed_publication_leaves_no_partial_entry() {
  let fixture = Fixture::new();
  ensure_directory(&fixture.cache.revision_directory).unwrap();
  ensure_directory(
    &fixture
      .cache
      .revision_directory
      .join(format!("{TARGET}.lock")),
  )
  .unwrap();
  assert!(fixture.cache.store(&Fixture::bundle()).is_err());
  assert!(!fixture.cache.directory.exists());
  fixture.assert_no_staging();
}

#[test]
fn a_busy_publication_lock_returns_without_waiting() {
  let fixture = Fixture::new();
  ensure_directory(&fixture.cache.revision_directory).unwrap();
  let path = fixture
    .cache
    .revision_directory
    .join(format!("{TARGET}.lock"));
  let held = PublicationLock::acquire(&path).unwrap();
  assert!(
    matches!(fixture.cache.store(&Fixture::bundle()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock)
  );
  assert!(fixture.cache.load().unwrap().is_none());
  fixture.assert_no_staging();
  drop(held);
  fixture.cache.store(&Fixture::bundle()).unwrap();
  assert!(fixture.cache.load().unwrap().is_some());
}

#[test]
fn staging_creation_never_reuses_an_existing_directory() {
  let fixture = Fixture::new();
  let path = fixture.directory.join("staging");
  create_private_directory(&path).unwrap();
  assert_eq!(
    create_private_directory(&path).unwrap_err().kind(),
    io::ErrorKind::AlreadyExists
  );
}

#[test]
fn a_damaged_entry_is_not_modified_when_new_bundle_validation_fails() {
  let fixture = Fixture::new();
  fixture.cache.store(&Fixture::bundle()).unwrap();
  fs::write(fixture.archive_path(), b"damaged").unwrap();
  let mut bundle = Fixture::bundle();
  bundle.archive = b"unverified".to_vec();
  assert!(fixture.cache.store(&bundle).is_err());
  assert_eq!(fs::read(fixture.archive_path()).unwrap(), b"damaged");
  fixture.assert_no_staging();
}

#[cfg(unix)]
#[test]
fn private_entries_allow_safe_existing_ancestors_and_preserve_flat_resources() {
  use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
  let fixture = Fixture::new();
  ensure_directory(&fixture.cache.root).unwrap();
  fs::set_permissions(&fixture.cache.root, fs::Permissions::from_mode(0o755)).unwrap();
  let flat = fixture.cache.root.join(BUNDLE_SET_FILE);
  fs::write(&flat, b"manual resource").unwrap();
  fixture.cache.store(&Fixture::bundle()).unwrap();
  assert_eq!(fs::read(flat).unwrap(), b"manual resource");
  assert_eq!(
    fs::metadata(&fixture.cache.root).unwrap().mode() & 0o777,
    0o755
  );
  for path in [&fixture.cache.revision_directory, &fixture.cache.directory] {
    assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o700);
  }
  for entry in fs::read_dir(&fixture.cache.directory).unwrap() {
    assert_eq!(entry.unwrap().metadata().unwrap().mode() & 0o777, 0o600);
  }
}

#[cfg(unix)]
#[test]
fn symlinks_special_files_and_unsafe_permissions_are_never_followed() {
  use std::os::unix::fs::{PermissionsExt as _, symlink};
  for index in 0..6 {
    let fixture = Fixture::new();
    fixture.cache.store(&Fixture::bundle()).unwrap();
    let outside = fixture.directory.join("outside");
    ensure_directory(&outside).unwrap();
    let path = match index {
      0 => fixture.cache.root.clone(),
      1 => fixture.cache.revision_directory.clone(),
      2 => fixture.cache.directory.clone(),
      3 => fixture.cache.directory.join(BUNDLE_SET_FILE),
      4 => fixture.archive_path(),
      _ => fixture
        .cache
        .revision_directory
        .join(format!("{TARGET}.lock")),
    };
    if path.is_dir() {
      fs::remove_dir_all(&path).unwrap();
    } else {
      fs::remove_file(&path).unwrap();
    }
    symlink(&outside, &path).unwrap();
    if index != 5 {
      assert!(fixture.cache.load().is_err());
    }
    assert!(fixture.cache.store(&Fixture::bundle()).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
  }
  let fixture = Fixture::new();
  fixture.cache.store(&Fixture::bundle()).unwrap();
  fs::set_permissions(&fixture.cache.directory, fs::Permissions::from_mode(0o777)).unwrap();
  assert!(fixture.cache.load().is_err());
  assert!(fixture.cache.store(&Fixture::bundle()).is_err());
}

#[cfg(unix)]
#[test]
fn fifo_cache_payloads_are_rejected_without_opening_or_blocking() {
  let fixture = Fixture::new();
  fixture.cache.store(&Fixture::bundle()).unwrap();
  let path = fixture.cache.directory.join(BUNDLE_SET_FILE);
  fs::remove_file(&path).unwrap();
  assert!(
    std::process::Command::new("mkfifo")
      .arg(&path)
      .status()
      .unwrap()
      .success()
  );
  assert!(fixture.cache.load().is_err());
  assert!(fixture.cache.store(&Fixture::bundle()).is_err());
}
