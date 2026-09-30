use super::*;
use std::path::PathBuf;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctld-public-hint-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn opaque(&self) -> IdentitySnapshot {
    let path = self.0.join("identity");
    std::fs::write(&path, "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMBcwAwYBKgQQeHh4eHh4eHh4eHh4eHh4eA==\n-----END ENCRYPTED PRIVATE KEY-----\n").unwrap();
    super::super::inspect_path(path.to_str().unwrap()).unwrap()
  }

  fn openssh(&self, public: &str) -> IdentitySnapshot {
    let blob = base64::engine::general_purpose::STANDARD
      .decode(public.split_ascii_whitespace().nth(1).unwrap())
      .unwrap();
    let mut bytes = b"openssh-key-v1\0".to_vec();
    for value in [b"aes256-ctr".as_slice(), b"bcrypt", b"fixture"] {
      string(&mut bytes, value);
    }
    bytes.extend_from_slice(&1_u32.to_be_bytes());
    string(&mut bytes, &blob);
    string(&mut bytes, &[0; 16]);
    let path = self.0.join("identity");
    std::fs::write(
      &path,
      format!(
        "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
        base64::engine::general_purpose::STANDARD.encode(bytes)
      ),
    )
    .unwrap();
    super::super::inspect_path(path.to_str().unwrap()).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn string(bytes: &mut Vec<u8>, value: &[u8]) {
  bytes.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
  bytes.extend_from_slice(value);
}

fn public(seed: u8) -> String {
  let mut blob = Vec::new();
  string(&mut blob, b"ssh-ed25519");
  string(&mut blob, &[seed; 32]);
  format!(
    "ssh-ed25519 {}",
    base64::engine::general_purpose::STANDARD.encode(blob)
  )
}

fn saved(snapshot: &IdentitySnapshot, key: &str) -> SavedIdentity {
  let public = parse(key).unwrap();
  SavedIdentity {
    version: 1,
    path: snapshot.path.clone(),
    file_version: snapshot.file_version.clone(),
    key_type: public.key_type,
    fingerprint: public.fingerprint,
    public_key: Some(public.canonical),
  }
}

#[test]
fn envelope_hint_is_canonical_and_never_queries_saved_metadata() {
  let fixture = Fixture::new();
  let public = public(1);
  let snapshot = fixture.openssh(&public);
  std::fs::write(
    format!("{}.pub", snapshot.path),
    public.clone().replace("ssh-ed25519", "ssh-rsa"),
  )
  .unwrap();
  assert_eq!(snapshot.public_key.as_deref(), Some(public.as_str()));
  assert_eq!(
    hint(&snapshot, None, || panic!("metadata read for envelope")),
    Some(public)
  );
}

#[test]
fn opaque_identity_uses_bounded_regular_public_sibling() {
  let fixture = Fixture::new();
  let snapshot = fixture.opaque();
  let public = public(2);
  let path = format!("{}.pub", snapshot.path);
  std::fs::write(&path, format!("{public} ignored comment\n")).unwrap();
  assert_eq!(
    hint(&snapshot, None, || panic!("metadata read for sibling")),
    Some(public.clone())
  );
  for invalid in [
    public.replace("ssh-ed25519", "ssh-rsa"),
    format!("{public}\n{public}"),
    "x".repeat(MAX_PUBLIC_KEY_BYTES + 1),
    "ssh-ed25519 invalid!".into(),
  ] {
    std::fs::write(&path, invalid).unwrap();
    assert!(hint(&snapshot, None, || None).is_none());
  }
  std::fs::remove_file(&path).unwrap();
  std::fs::create_dir(&path).unwrap();
  assert!(hint(&snapshot, None, || None).is_none());
}

#[test]
fn saved_hint_requires_matching_file_binding_and_public_metadata() {
  let fixture = Fixture::new();
  let snapshot = fixture.opaque();
  let public = public(3);
  let metadata = saved(&snapshot, &public);
  assert_eq!(saved_hint(&snapshot, &metadata), Some(public.clone()));
  let mut changed = metadata.clone();
  changed.file_version = "a".repeat(64);
  assert!(saved_hint(&snapshot, &changed).is_none());
  let mut changed = metadata.clone();
  changed.path.push_str("-different");
  assert!(saved_hint(&snapshot, &changed).is_none());
  let mut changed = metadata.clone();
  changed.fingerprint = "SHA256:wrong".into();
  assert!(saved_hint(&snapshot, &changed).is_none());
  let mut changed = metadata;
  changed.key_type = "ssh-rsa".into();
  assert!(saved_hint(&snapshot, &changed).is_none());
}

#[test]
fn stale_public_sibling_does_not_override_verified_saved_hint() {
  let fixture = Fixture::new();
  let snapshot = fixture.opaque();
  let public = public(4);
  std::fs::write(format!("{}.pub", snapshot.path), self::public(5)).unwrap();
  assert_eq!(
    saved_hint(&snapshot, &saved(&snapshot, &public)),
    Some(public)
  );
}

#[test]
fn legacy_metadata_uses_envelope_but_cannot_guess_an_opaque_public_key() {
  let fixture = Fixture::new();
  let public = public(6);
  let snapshot = fixture.openssh(&public);
  let mut metadata = saved(&snapshot, &public);
  metadata.public_key = None;
  let encoded = serde_json::to_string(&metadata).unwrap();
  assert!(!encoded.contains("public_key"));
  let decoded: SavedIdentity = serde_json::from_str(&encoded).unwrap();
  assert_eq!(saved_hint(&snapshot, &decoded), Some(public.clone()));
  let snapshot = fixture.opaque();
  let mut metadata = saved(&snapshot, &public);
  metadata.public_key = None;
  assert!(saved_hint(&snapshot, &metadata).is_none());
}

#[test]
fn replacing_private_file_invalidates_all_hint_sources() {
  let fixture = Fixture::new();
  let public = public(7);
  let snapshot = fixture.openssh(&public);
  let metadata = saved(&snapshot, &public);
  let _replacement = fixture.opaque();
  assert!(hint(&snapshot, None, || panic!("stale snapshot read metadata")).is_none());
  assert!(saved_hint(&snapshot, &metadata).is_none());
}

#[cfg(unix)]
#[test]
fn a_public_fifo_never_blocks_hint_discovery() {
  let fixture = Fixture::new();
  let snapshot = fixture.opaque();
  assert!(
    std::process::Command::new("mkfifo")
      .arg(format!("{}.pub", snapshot.path))
      .status()
      .unwrap()
      .success()
  );
  assert!(hint(&snapshot, None, || None).is_none());
}
