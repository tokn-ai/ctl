#[path = "../bundle_build.rs"]
mod bundle_build;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::path::PathBuf;

const TARGET: &str = "aarch64-apple-darwin";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ARCHIVE: &[u8] = b"release archive fixture";

struct Fixture {
  root: PathBuf,
  source: PathBuf,
  output: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctl-bundle-build-{}", uuid::Uuid::new_v4()));
    let source = root.join("source");
    let output = root.join("output");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir(&output).unwrap();
    let fixture = Self {
      root,
      source,
      output,
    };
    fixture.write_manifest(&manifest());
    fs::write(fixture.archive_path(), ARCHIVE).unwrap();
    fixture
  }

  fn manifest_path(&self) -> PathBuf {
    self.source.join(format!("ctld-{TARGET}.json"))
  }

  fn archive_path(&self) -> PathBuf {
    self
      .source
      .join(format!("ctld-{VERSION}-{TARGET}.app.tar.gz"))
  }

  fn write_manifest(&self, manifest: &Value) {
    fs::write(self.manifest_path(), serde_json::to_vec(manifest).unwrap()).unwrap();
  }

  fn reject(&self) {
    assert!(
      bundle_build::stage(
        &self.source,
        &self.output,
        VERSION,
        TARGET,
        bundle_build::Mode::Signed
      )
      .is_err()
    );
    assert_eq!(fs::read_dir(&self.output).unwrap().count(), 0);
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.root);
  }
}

fn manifest() -> Value {
  json!({
    "schema_version": 1,
    "component": "ctld",
    "app_version": VERSION,
    "bundle_id": VERSION,
    "git_revision": "a".repeat(40),
    "target": TARGET,
    "bundle_identifier": "dev.tokn-ai.ctl.ctld",
    "team_identifier": "ABCDEFGHIJ",
    "signing_mode": "signed",
    "notarized": true,
    "archive": format!("ctld-{VERSION}-{TARGET}.app.tar.gz"),
    "sha256": format!("{:x}", Sha256::digest(ARCHIVE)),
    "archive_size": ARCHIVE.len(),
  })
}

#[test]
fn staged_payload_is_a_snapshot_of_the_matching_release() {
  let fixture = Fixture::new();
  let original_manifest = fs::read(fixture.manifest_path()).unwrap();
  bundle_build::stage(
    &fixture.source,
    &fixture.output,
    VERSION,
    TARGET,
    bundle_build::Mode::Signed,
  )
  .unwrap();
  fs::write(fixture.manifest_path(), b"another build's manifest").unwrap();
  fs::write(fixture.archive_path(), b"another build's archive").unwrap();
  assert_eq!(
    fs::read(fixture.output.join("ctld-manifest.json")).unwrap(),
    original_manifest
  );
  assert_eq!(
    fs::read(fixture.output.join("ctld.app.tar.gz")).unwrap(),
    ARCHIVE
  );
}

fn development_manifest() -> Value {
  let mut manifest = manifest();
  manifest["bundle_id"] = json!(format!("dev.{}", manifest["sha256"].as_str().unwrap()));
  manifest["signing_mode"] = json!("development");
  manifest["notarized"] = json!(false);
  manifest["development"] = json!({ "source_fingerprint": "b".repeat(64), "dirty": true });
  manifest
}

#[test]
fn development_bundles_require_explicit_mode_and_cannot_replace_release_payloads() {
  let fixture = Fixture::new();
  fixture.write_manifest(&development_manifest());
  fixture.reject();
  bundle_build::stage(
    &fixture.source,
    &fixture.output,
    VERSION,
    TARGET,
    bundle_build::Mode::Development,
  )
  .unwrap();
  assert_eq!(
    fs::read(fixture.output.join("ctld.app.tar.gz")).unwrap(),
    ARCHIVE
  );

  let fixture = Fixture::new();
  assert!(
    bundle_build::stage(
      &fixture.source,
      &fixture.output,
      VERSION,
      TARGET,
      bundle_build::Mode::Development
    )
    .is_err()
  );
  let mut release = manifest();
  release["development"] = development_manifest()["development"].clone();
  fixture.write_manifest(&release);
  fixture.reject();
}

#[test]
fn development_bundles_bind_the_cache_id_and_source_fingerprint() {
  for (field, value) in [
    ("bundle_id", json!("dev.other-archive")),
    ("notarized", json!(true)),
    ("development", json!(null)),
    (
      "development",
      json!({ "source_fingerprint": "invalid", "dirty": true }),
    ),
    (
      "development",
      json!({ "source_fingerprint": "b".repeat(64), "dirty": "true" }),
    ),
    (
      "development",
      json!({ "source_fingerprint": "b".repeat(64), "dirty": true, "trust_override": true }),
    ),
  ] {
    let fixture = Fixture::new();
    let mut manifest = development_manifest();
    manifest[field] = value;
    fixture.write_manifest(&manifest);
    assert!(
      bundle_build::stage(
        &fixture.source,
        &fixture.output,
        VERSION,
        TARGET,
        bundle_build::Mode::Development
      )
      .is_err()
    );
    assert_eq!(fs::read_dir(&fixture.output).unwrap().count(), 0);
  }
}

#[test]
fn other_releases_unsigned_helpers_and_path_overrides_are_rejected_before_staging() {
  for (field, value) in [
    ("app_version", json!("9.9.9")),
    ("bundle_id", json!("development-build")),
    ("target", json!("x86_64-apple-darwin")),
    ("bundle_identifier", json!("dev.other.helper")),
    ("team_identifier", json!("injected\"team")),
    ("signing_mode", json!("unsigned")),
    ("notarized", json!(false)),
    ("archive", json!("../unverified.app.tar.gz")),
    ("archive_size", json!(128_u64 * 1024 * 1024 + 1)),
    ("unknown_policy_override", json!(true)),
    ("development", json!(null)),
  ] {
    let fixture = Fixture::new();
    let mut invalid = manifest();
    invalid[field] = value;
    fixture.write_manifest(&invalid);
    fixture.reject();
  }
}

#[test]
fn checksum_and_truncated_archive_failures_do_not_leave_partial_snapshots() {
  for archive in [
    b"incorrect archive bytes".as_slice(),
    &ARCHIVE[..ARCHIVE.len() - 1],
  ] {
    let fixture = Fixture::new();
    fs::write(fixture.archive_path(), archive).unwrap();
    fixture.reject();
  }
}

#[test]
fn oversized_manifests_and_unsupported_targets_are_rejected() {
  let fixture = Fixture::new();
  fs::write(fixture.manifest_path(), vec![b' '; 16 * 1024 + 1]).unwrap();
  fixture.reject();
  assert!(
    bundle_build::stage(
      &fixture.source,
      &fixture.output,
      VERSION,
      "x86_64-unknown-linux-gnu",
      bundle_build::Mode::Signed,
    )
    .is_err()
  );
}

#[cfg(unix)]
#[test]
fn symlinked_manifests_and_archives_are_rejected() {
  for archive in [false, true] {
    let fixture = Fixture::new();
    let path = if archive {
      fixture.archive_path()
    } else {
      fixture.manifest_path()
    };
    let original = fixture.root.join("external-payload");
    fs::rename(&path, &original).unwrap();
    std::os::unix::fs::symlink(original, path).unwrap();
    fixture.reject();
  }
}

#[cfg(unix)]
#[test]
fn fifo_payloads_are_rejected_without_waiting_for_a_writer() {
  for archive in [false, true] {
    let fixture = Fixture::new();
    let path = if archive {
      fixture.archive_path()
    } else {
      fixture.manifest_path()
    };
    fs::remove_file(&path).unwrap();
    assert!(
      std::process::Command::new("/usr/bin/mkfifo")
        .arg(path)
        .status()
        .unwrap()
        .success()
    );
    let source = fixture.source.clone();
    let output = fixture.output.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      sender
        .send(bundle_build::stage(
          &source,
          &output,
          VERSION,
          TARGET,
          bundle_build::Mode::Signed,
        ))
        .unwrap();
    });
    assert!(
      receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap()
        .is_err()
    );
    assert_eq!(fs::read_dir(&fixture.output).unwrap().count(), 0);
  }
}
