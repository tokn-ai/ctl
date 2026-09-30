use super::*;
use std::path::PathBuf;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctld-identity-fixture-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn write(&self, name: &str, encrypted: bool) -> String {
    let path = self.0.join(name);
    let header = if encrypted {
      "ENCRYPTED PRIVATE KEY"
    } else {
      "PRIVATE KEY"
    };
    let body = if encrypted {
      "MBcwAwYBKgQQeHh4eHh4eHh4eHh4eHh4eA=="
    } else {
      "MAsCAQAwAwYBKgQBeA=="
    };
    std::fs::write(
      &path,
      format!("-----BEGIN {header}-----\n{body}\n-----END {header}-----\n"),
    )
    .unwrap();
    path.to_string_lossy().into_owned()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn changed_and_missing_keys_do_not_reuse_saved_state() {
  let fixture = Fixture::new();
  let path = fixture.write("encrypted", true);
  let snapshot = inspect_path(&path).unwrap();
  let metadata = SavedIdentity {
    version: 1,
    path: snapshot.path.clone(),
    file_version: snapshot.file_version.clone(),
    key_type: "ssh-ed25519".into(),
    fingerprint: "SHA256:fixture".into(),
  };
  let saved = HashMap::from([(snapshot.identity_id.clone(), metadata)]);
  let mut complete = true;
  assert_eq!(
    project(std::slice::from_ref(&path), &saved, true, &mut complete)[0].passphrase_state,
    PassphraseState::Saved
  );
  std::fs::write(
    &path,
    "-----BEGIN ENCRYPTED PRIVATE KEY-----\ncmVwbGFjZWQ=\n-----END ENCRYPTED PRIVATE KEY-----\n",
  )
  .unwrap();
  assert_eq!(
    project(std::slice::from_ref(&path), &saved, true, &mut complete)[0].passphrase_state,
    PassphraseState::FileChanged
  );
  assert!(matches!(
    super::super::ensure_current(&snapshot),
    Err(IdentityError::FileChanged)
  ));
  std::fs::remove_file(&path).unwrap();
  let records = project(std::slice::from_ref(&path), &saved, true, &mut complete);
  assert_eq!(records[0].file_state, FileState::Missing);
  assert_eq!(records[0].passphrase_state, PassphraseState::FileChanged);
  assert!(records[0].fingerprint.is_none());
  assert!(records[0].key_type.is_none());
}

#[cfg(unix)]
#[test]
fn canonical_paths_deduplicate_symlinks_and_plain_keys_need_no_passphrase() {
  let fixture = Fixture::new();
  let path = fixture.write("plain", false);
  let symlink = fixture.0.join("alias");
  std::os::unix::fs::symlink(&path, &symlink).unwrap();
  let mut complete = true;
  let records = project(
    &[path, symlink.to_string_lossy().into_owned()],
    &HashMap::new(),
    true,
    &mut complete,
  );
  assert_eq!(records.len(), 1);
  assert_eq!(records[0].passphrase_state, PassphraseState::NotRequired);
  assert!(complete);
}

#[test]
fn metadata_failures_remain_explicit_and_secret_bytes_never_serialize() {
  let fixture = Fixture::new();
  let path = fixture.write("encrypted", true);
  let mut complete = true;
  let records = project(&[path], &HashMap::new(), false, &mut complete);
  assert_eq!(records[0].passphrase_state, PassphraseState::Unknown);
  let json = serde_json::to_string(&records).unwrap();
  assert!(!json.contains("ZmFrZQ"));
  assert!(!json.contains("PRIVATE KEY"));
}

#[test]
fn envelope_metadata_is_not_presented_as_a_verified_identity() {
  let fixture = Fixture::new();
  let path = fixture.write("encrypted", true);
  let mut snapshot = inspect_path(&path).unwrap();
  snapshot.key_type = Some("ssh-ed25519".into());
  snapshot.fingerprint = Some("SHA256:unverified-envelope".into());
  let record = snapshot.record(true);
  assert!(record.key_type.is_none());
  assert!(record.fingerprint.is_none());
  assert_eq!(record.passphrase_state, PassphraseState::NotSaved);
}

#[test]
fn nonregular_oversized_and_public_files_are_unsupported() {
  let fixture = Fixture::new();
  assert!(matches!(
    inspect_path(fixture.0.to_str().unwrap()),
    Err(IdentityError::UnsupportedFile)
  ));
  let path = fixture.0.join("oversized");
  std::fs::write(&path, vec![b'x'; files::MAX_KEY_BYTES + 1]).unwrap();
  assert!(matches!(
    inspect_path(path.to_str().unwrap()),
    Err(IdentityError::UnsupportedFile)
  ));
  std::fs::write(&path, "ssh-ed25519 AAAA public").unwrap();
  assert!(matches!(
    inspect_path(path.to_str().unwrap()),
    Err(IdentityError::UnsupportedFile)
  ));
}

#[test]
fn saved_metadata_cannot_redirect_an_identity_to_another_path() {
  let metadata = SavedIdentity {
    version: 1,
    path: "/tmp/key-fixture".into(),
    file_version: "a".repeat(64),
    key_type: "ssh-rsa".into(),
    fingerprint: "SHA256:fixture".into(),
  };
  assert!(metadata.valid(&files::digest(metadata.path.as_bytes())));
  assert!(!metadata.valid(&"b".repeat(64)));
}
