use super::{
  archive,
  install::Session,
  manifest::{self, Manifest},
};
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
use std::path::PathBuf;

pub(super) struct Home(pub(super) PathBuf);

impl Home {
  pub(super) fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctld-setup-test-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }
}

impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

const FILES: [&str; 5] = [
  "ctld.app/Contents/Info.plist",
  "ctld.app/Contents/embedded.provisionprofile",
  "ctld.app/Contents/_CodeSignature/CodeResources",
  "ctld.app/Contents/MacOS/ctld",
  "ctld.app/Contents/CodeResources",
];

fn append(builder: &mut tar::Builder<Vec<u8>>, path: &str, kind: tar::EntryType) {
  let mut header = tar::Header::new_ustar();
  // Writing raw names lets the tests represent unsafe archives rejected by the
  // normal tar builder's path API.
  header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
  header.set_mode(0o7777);
  header.set_entry_type(kind);
  if kind.is_symlink() || kind.is_hard_link() {
    header.set_link_name("/tmp/elsewhere").unwrap();
  }
  let data: &[u8] = if kind.is_file() { b"fixture" } else { b"" };
  header.set_size(data.len() as u64);
  header.set_cksum();
  builder.append(&header, data).unwrap();
}

pub(super) fn compressed(raw: &[u8]) -> Vec<u8> {
  let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
  encoder.write_all(raw).unwrap();
  encoder.finish().unwrap()
}

pub(super) fn contents(extra: Option<(&str, tar::EntryType)>) -> Vec<u8> {
  let mut builder = tar::Builder::new(Vec::new());
  for path in FILES {
    append(&mut builder, path, tar::EntryType::Regular);
  }
  if let Some((path, kind)) = extra {
    append(&mut builder, path, kind);
  }
  builder.into_inner().unwrap()
}

pub(super) fn release(bytes: &[u8]) -> Manifest {
  let mut manifest = manifest::fixture();
  manifest.archive_size = bytes.len() as u64;
  manifest.sha256 = format!("{:x}", Sha256::digest(bytes));
  manifest
}

pub(super) fn development_bundle() -> (Vec<u8>, Manifest) {
  let mut builder = tar::Builder::new(Vec::new());
  for path in &FILES[..4] {
    append(&mut builder, path, tar::EntryType::Regular);
  }
  let bytes = compressed(&builder.into_inner().unwrap());
  let mut manifest = release(&bytes);
  manifest.signing_mode = "development".into();
  manifest.notarized = false;
  manifest.bundle_id = format!("dev.{}", manifest.sha256);
  manifest.development = Some(manifest::Development {
    source_fingerprint: "c".repeat(64),
    dirty: true,
  });
  (bytes, manifest)
}

#[test]
fn development_installs_reuse_digest_cache_without_replacing_production_selection() {
  let home = Home::new();
  let production = compressed(&contents(None));
  let installed = Session::begin(&home.0, release(&production))
    .unwrap()
    .unpack(&production)
    .unwrap()
    .activate()
    .unwrap();
  let root = ctl_ipc::managed::component_directory(&home.0);
  let selected = fs::read_link(root.join("current")).unwrap();
  let (bytes, manifest) = development_bundle();
  let development = Session::begin(&home.0, manifest.clone())
    .unwrap()
    .unpack(&bytes)
    .unwrap()
    .activate()
    .unwrap();
  assert_eq!(
    development.executable,
    root
      .join("development")
      .join(&manifest.sha256)
      .join("ctld.app/Contents/MacOS/ctld")
      .canonicalize()
      .unwrap()
  );
  assert_eq!(fs::read_link(root.join("current")).unwrap(), selected);
  assert_eq!(
    ctl_ipc::managed::resolve_executable(&home.0)
      .unwrap()
      .unwrap(),
    installed.executable
  );
  assert!(Session::begin(&home.0, manifest.clone()).unwrap().reused);
  let mut changed = manifest;
  changed.development.as_mut().unwrap().dirty = false;
  assert!(Session::begin(&home.0, changed).is_err());
}

#[test]
fn development_bundle_can_omit_the_staple_but_release_policy_still_requires_it() {
  let (bytes, development) = development_bundle();
  let home = Home::new();
  archive::extract(&bytes, &development, &home.0).unwrap();
  let home = Home::new();
  assert!(archive::extract(&bytes, &release(&bytes), &home.0).is_err());
}

#[test]
fn extracts_complete_bundle_with_safe_permissions() {
  let home = Home::new();
  let bytes = compressed(&contents(None));
  archive::extract(&bytes, &release(&bytes), &home.0).unwrap();
  for path in FILES {
    let metadata = fs::metadata(home.0.join(path)).unwrap();
    assert_eq!(
      metadata.permissions().mode() & 0o7777,
      if path.ends_with("MacOS/ctld") {
        0o755
      } else {
        0o644
      }
    );
  }
}

#[test]
fn rejects_traversal_links_extensions_duplicates_and_special_files() {
  for (path, kind) in [
    ("../escape", tar::EntryType::Regular),
    ("/ctld.app/escape", tar::EntryType::Regular),
    ("ctld.app/../escape", tar::EntryType::Regular),
    ("other.app/Contents/file", tar::EntryType::Regular),
    (FILES[0], tar::EntryType::Regular),
    ("ctld.app/link", tar::EntryType::Symlink),
    ("ctld.app/hardlink", tar::EntryType::Link),
    ("ctld.app/fifo", tar::EntryType::Fifo),
    ("ctld.app/device", tar::EntryType::Char),
    ("ctld.app/pax", tar::EntryType::XHeader),
    ("ctld.app/sparse", tar::EntryType::GNUSparse),
  ] {
    let home = Home::new();
    let bytes = compressed(&contents(Some((path, kind))));
    assert!(
      archive::extract(&bytes, &release(&bytes), &home.0).is_err(),
      "{path}"
    );
  }
}

#[test]
fn rejects_checksums_truncation_corrupt_gzip_and_trailing_payload() {
  let raw = contents(None);
  let valid = compressed(&raw);
  let mut wrong_checksum = release(&valid);
  wrong_checksum.sha256 = "0".repeat(64);
  let home = Home::new();
  assert!(archive::extract(&valid, &wrong_checksum, &home.0).is_err());
  assert!(!home.0.join("ctld.app").exists());
  let truncated = compressed(&raw[..600]);
  let mut corrupt = valid.clone();
  let index = corrupt.len() - 8;
  corrupt[index] ^= 1;
  let mut appended = raw.clone();
  appended.extend_from_slice(b"unexpected payload");
  for bytes in [
    truncated,
    corrupt,
    compressed(&appended),
    valid[..valid.len() - 1].to_vec(),
  ] {
    let home = Home::new();
    assert!(archive::extract(&bytes, &release(&bytes), &home.0).is_err());
  }
}

#[test]
fn rejects_excessive_entry_count_and_missing_ticket() {
  let mut builder = tar::Builder::new(Vec::new());
  for index in 0..257 {
    append(
      &mut builder,
      &format!("ctld.app/file-{index}"),
      tar::EntryType::Regular,
    );
  }
  let excessive = compressed(&builder.into_inner().unwrap());
  let mut builder = tar::Builder::new(Vec::new());
  for path in &FILES[..4] {
    append(&mut builder, path, tar::EntryType::Regular);
  }
  let missing = compressed(&builder.into_inner().unwrap());
  for bytes in [excessive, missing] {
    let home = Home::new();
    assert!(archive::extract(&bytes, &release(&bytes), &home.0).is_err());
  }
}

#[test]
fn incomplete_setup_cleans_staging_and_preserves_previous_selection() {
  let home = Home::new();
  let bytes = compressed(&contents(None));
  let manifest = release(&bytes);
  let installed = Session::begin(&home.0, manifest.clone())
    .unwrap()
    .unpack(&bytes)
    .unwrap()
    .activate()
    .unwrap();
  let root = ctl_ipc::managed::component_directory(&home.0);
  let original = fs::read_link(root.join("current")).unwrap();
  let mut next = manifest;
  next.app_version = "0.2.0".into();
  next.bundle_id = "0.2.0".into();
  next.archive = "ctld-0.2.0-aarch64-apple-darwin.app.tar.gz".into();
  let session = Session::begin(&home.0, next)
    .unwrap()
    .unpack(&bytes)
    .unwrap();
  let staging = session.work().to_owned();
  // Covers cancellation and failed verification: neither invokes activation.
  drop(session);
  assert!(!staging.exists());
  assert_eq!(fs::read_link(root.join("current")).unwrap(), original);
  assert_eq!(
    ctl_ipc::managed::resolve_executable(&home.0)
      .unwrap()
      .unwrap(),
    installed.executable
  );
}

#[test]
fn concurrent_setup_and_changed_release_are_rejected() {
  let home = Home::new();
  let bytes = compressed(&contents(None));
  let manifest = release(&bytes);
  let session = Session::begin(&home.0, manifest.clone()).unwrap();
  assert!(matches!(
    Session::begin(&home.0, manifest.clone()),
    Err(super::Error::Busy)
  ));
  session.unpack(&bytes).unwrap().activate().unwrap();
  assert!(Session::begin(&home.0, manifest.clone()).unwrap().reused);
  let mut changed = manifest;
  changed.sha256 = "0".repeat(64);
  assert!(Session::begin(&home.0, changed).is_err());
}

#[test]
fn invalid_existing_selection_is_never_replaced() {
  let home = Home::new();
  let root = ctl_ipc::managed::ensure_component_directory(&home.0).unwrap();
  symlink("../../outside", root.join("current")).unwrap();
  let bytes = compressed(&contents(None));
  assert!(Session::begin(&home.0, release(&bytes)).is_err());
  assert_eq!(
    fs::read_link(root.join("current")).unwrap(),
    PathBuf::from("../../outside")
  );
}

#[test]
fn setup_repairs_a_dangling_owned_selection_after_candidate_validation() {
  let home = Home::new();
  let root = ctl_ipc::managed::ensure_component_directory(&home.0).unwrap();
  symlink("versions/missing", root.join("current")).unwrap();
  let bytes = compressed(&contents(None));
  let installed = Session::begin(&home.0, release(&bytes))
    .unwrap()
    .unpack(&bytes)
    .unwrap()
    .activate()
    .unwrap();
  assert_eq!(
    ctl_ipc::managed::resolve_executable(&home.0)
      .unwrap()
      .unwrap(),
    installed.executable
  );
}

#[test]
fn reused_bundle_cannot_poison_current_or_follow_a_symlink() {
  let home = Home::new();
  let bytes = compressed(&contents(None));
  let manifest = release(&bytes);
  Session::begin(&home.0, manifest.clone())
    .unwrap()
    .unpack(&bytes)
    .unwrap()
    .activate()
    .unwrap();
  let root = ctl_ipc::managed::component_directory(&home.0);
  fs::remove_file(root.join("current")).unwrap();
  let version = root.join("versions").join(manifest.directory_name());
  let original = home.0.join("external.app");
  fs::rename(version.join("ctld.app"), &original).unwrap();
  symlink(&original, version.join("ctld.app")).unwrap();
  let session = Session::begin(&home.0, manifest).unwrap();
  assert!(session.executable().is_err());
  assert!(session.activate().is_err());
  assert!(root.join("current").symlink_metadata().is_err());
  assert!(original.join("Contents/MacOS/ctld").exists());
}

#[test]
fn cached_marker_fifo_is_rejected_without_waiting_for_a_writer() {
  let home = Home::new();
  let bytes = compressed(&contents(None));
  let manifest = release(&bytes);
  Session::begin(&home.0, manifest.clone())
    .unwrap()
    .unpack(&bytes)
    .unwrap()
    .activate()
    .unwrap();
  let root = ctl_ipc::managed::component_directory(&home.0);
  let marker = root
    .join("versions")
    .join(manifest.directory_name())
    .join("installation.json");
  fs::remove_file(&marker).unwrap();
  assert!(
    std::process::Command::new("/usr/bin/mkfifo")
      .arg(&marker)
      .status()
      .unwrap()
      .success()
  );
  assert!(Session::begin(&home.0, manifest).is_err());
}
