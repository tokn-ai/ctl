use super::*;
use crate::setup::{archive, tests::Home};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};

struct Fixture {
  home: Home,
  repository: PathBuf,
  checkpoint: PathBuf,
  directory: PathBuf,
  manifest: Manifest,
}

impl Fixture {
  fn new() -> Self {
    let (archive, manifest) = crate::setup::tests::development_bundle();
    Self::with_bundle(&archive, manifest)
  }

  fn with_bundle(bytes: &[u8], manifest: Manifest) -> Self {
    let home = Home::new();
    let repository = home.0.join("repository");
    fs::DirBuilder::new()
      .mode(0o700)
      .create(&repository)
      .unwrap();
    let repository = repository.canonicalize().unwrap();
    let digest = format!(
      "{:x}",
      Sha256::digest(repository.to_str().unwrap().as_bytes())
    );
    let target = home.0.join("target");
    let checkpoint = target.join("ctl-dev/helpers").join(&digest[..20]);
    let directory = checkpoint.join(format!("build-{}", manifest.sha256));
    fs::DirBuilder::new()
      .recursive(true)
      .mode(0o700)
      .create(&directory)
      .unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    archive::extract(bytes, &manifest, &directory).unwrap();
    let fixture = Self {
      home,
      repository,
      checkpoint,
      directory,
      manifest,
    };
    fixture.write_receipt();
    fixture.select();
    fixture
  }

  fn write_receipt(&self) {
    let file = fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(self.directory.join("ctld-package.json"))
      .unwrap();
    serde_json::to_writer(file, &self.manifest).unwrap();
  }

  fn select(&self) {
    self.write_selection(&serde_json::json!({
      "schema_version": 1,
      "repository_root": self.repository,
      "target": self.manifest.target,
      "directory": format!("build-{}", self.manifest.sha256),
    }));
  }

  fn write_selection(&self, selection: &serde_json::Value) {
    let file = fs::OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(self.checkpoint.join("selected.json"))
      .unwrap();
    serde_json::to_writer(file, selection).unwrap();
  }

  fn selection(&self) -> serde_json::Value {
    serde_json::from_slice(&fs::read(self.checkpoint.join("selected.json")).unwrap()).unwrap()
  }

  fn load(&self) -> Result<Option<Candidate>, Error> {
    load_candidate(&self.checkpoint, &self.repository, &self.manifest.target)
  }
}

#[test]
fn missing_checkpoint_and_selection_do_not_create_state() {
  let fixture = Fixture::new();
  fs::remove_dir_all(fixture.home.0.join("target")).unwrap();
  fs::remove_dir_all(&fixture.repository).unwrap();
  assert!(fixture.load().unwrap().is_none());
  assert!(!fixture.home.0.join("target").exists());
  let fixture = Fixture::new();
  fs::remove_file(fixture.checkpoint.join("selected.json")).unwrap();
  assert!(fixture.load().unwrap().is_none());
  assert!(!fixture.checkpoint.join("selected.json").exists());
}

#[test]
fn source_checkout_presence_is_not_required_for_provisioned_helper_discovery() {
  let fixture = Fixture::new();
  fs::remove_dir_all(&fixture.repository).unwrap();
  let candidate = fixture.load().unwrap().unwrap();
  assert_eq!(
    candidate.directory,
    fixture.directory.canonicalize().unwrap()
  );
}

#[test]
fn ordinary_target_permissions_allow_private_verified_build_discovery() {
  let fixture = Fixture::new();
  let candidate = fixture.load().unwrap().unwrap();
  assert_eq!(
    candidate.directory,
    fixture.directory.canonicalize().unwrap()
  );
  assert_eq!(
    candidate.receipt,
    serde_json::to_vec(&fixture.manifest).unwrap()
  );
  assert_eq!(
    fs::metadata(fixture.home.0.join("target")).unwrap().mode() & 0o777,
    0o755
  );
}

#[test]
fn selectors_are_bound_to_worktree_architecture_and_schema() {
  for field in ["repository_root", "target", "schema_version", "unexpected"] {
    let fixture = Fixture::new();
    let mut selection = fixture.selection();
    selection[field] = match field {
      "schema_version" => 2.into(),
      "target" => "x86_64-apple-darwin".into(),
      "repository_root" => fixture
        .repository
        .join("other-worktree")
        .to_str()
        .unwrap()
        .into(),
      _ => "unrecognized".into(),
    };
    fixture.write_selection(&selection);
    assert!(fixture.load().is_err(), "field {field}");
  }
  let fixture = Fixture::new();
  let foreign_checkpoint = fixture.checkpoint.with_file_name("0".repeat(20));
  fs::rename(&fixture.checkpoint, &foreign_checkpoint).unwrap();
  assert!(
    load_candidate(
      &foreign_checkpoint,
      &fixture.repository,
      &fixture.manifest.target
    )
    .is_err()
  );
}

#[test]
fn selections_never_traverse_outside_the_immutable_build_cache() {
  for directory in [
    "../outside".to_owned(),
    "/outside".to_owned(),
    "build-short".to_owned(),
    format!("build-{}/", "a".repeat(64)),
    format!("build-{}", "A".repeat(64)),
    format!("build-{}/../outside", "a".repeat(64)),
  ] {
    let fixture = Fixture::new();
    let mut selection = fixture.selection();
    selection["directory"] = directory.clone().into();
    fixture.write_selection(&selection);
    assert!(fixture.load().is_err(), "directory {directory}");
  }
}

#[test]
fn receipt_requires_development_policy_and_matching_archive_identity() {
  let mut fixture = Fixture::new();
  fixture.manifest.development = None;
  fixture.manifest.signing_mode = "signed".into();
  fixture.manifest.notarized = true;
  fixture.manifest.bundle_id = fixture.manifest.app_version.clone();
  fixture.write_receipt();
  assert!(fixture.load().is_err());
  let mut fixture = Fixture::new();
  fixture.manifest.sha256 = "f".repeat(64);
  fixture.manifest.bundle_id = format!("dev.{}", fixture.manifest.sha256);
  fixture.write_receipt();
  assert!(fixture.load().is_err());
}

#[test]
fn selector_and_receipt_are_bounded_regular_files() {
  for (relative, limit) in [
    ("selected.json", MAX_SELECTION_BYTES),
    (
      "ctld-package.json",
      crate::setup::manifest::MAX_MANIFEST_BYTES,
    ),
  ] {
    let fixture = Fixture::new();
    let path = if relative == "selected.json" {
      fixture.checkpoint.join(relative)
    } else {
      fixture.directory.join(relative)
    };
    fs::write(path, vec![b' '; limit + 1]).unwrap();
    assert!(fixture.load().is_err(), "oversized {relative}");
  }
}

#[test]
fn permissive_cache_or_package_permissions_are_rejected() {
  for (relative, mode) in [
    ("", 0o755),
    ("selected.json", 0o666),
    ("ctld.app/Contents/MacOS/ctld", 0o775),
    ("ctld-package.json", 0o666),
  ] {
    let fixture = Fixture::new();
    let path = match relative {
      "" => fixture.checkpoint.clone(),
      "selected.json" => fixture.checkpoint.join(relative),
      _ => fixture.directory.join(relative),
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    assert!(fixture.load().is_err(), "permissions for {relative}");
  }
  let fixture = Fixture::new();
  fs::set_permissions(
    fixture.home.0.join("target"),
    fs::Permissions::from_mode(0o777),
  )
  .unwrap();
  assert!(fixture.load().is_err());
}

#[test]
fn symlinked_checkpoints_selections_builds_and_app_entries_are_rejected() {
  for kind in ["checkpoint", "selection", "build", "app-entry"] {
    let fixture = Fixture::new();
    let path = match kind {
      "checkpoint" => fixture.checkpoint.clone(),
      "selection" => fixture.checkpoint.join("selected.json"),
      "build" => fixture.directory.clone(),
      _ => fixture.directory.join("ctld.app/Contents/Info.plist"),
    };
    let original = fixture.home.0.join("moved");
    fs::rename(&path, &original).unwrap();
    symlink(original, path).unwrap();
    assert!(fixture.load().is_err(), "symlinked {kind}");
  }
}

#[cfg(target_os = "macos")]
#[test]
fn valid_checkout_helpers_still_require_the_exact_operation_contract() {
  let fixture = Fixture::new();
  let mut info = ctl_core::component::ComponentInfo {
    build: ctl_core::component::build_info(),
    protocols: fixture.manifest.protocols,
  };
  let old = ctl_ipc::HELPER_API_CONTRACT_V1_0_1;
  let clear = ctl_ipc::HELPER_API_CONTRACT_V1_1_3;
  assert!(crate::setup::discovery::compatible_for_helper_contract(
    &info,
    Some(clear)
  ));
  *info
    .protocols
    .iter_mut()
    .find(|entry| entry.name == "ctld_helper")
    .unwrap() = ctl_core::component::ProtocolInfo::new("ctld_helper", old.build, old, &[old]);
  assert!(crate::setup::discovery::compatible_for_helper_contract(
    &info, None
  ));
  assert!(!crate::setup::discovery::compatible_for_helper_contract(
    &info,
    Some(clear)
  ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn unsigned_checkout_helper_is_rejected_before_metadata_execution() {
  let fixture = Fixture::new();
  let marker = fixture.home.0.join("metadata-executed");
  let executable = fixture.directory.join("ctld.app/Contents/MacOS/ctld");
  fs::write(
    &executable,
    format!("#!/bin/sh\n/usr/bin/touch '{}'\n", marker.display()),
  )
  .unwrap();
  let candidate = fixture.load().unwrap().unwrap();
  assert!(
    verify_candidate(&fixture.home.0, &candidate, None)
      .await
      .is_err()
  );
  assert!(!marker.exists());
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires a provisioned signed development helper"]
async fn provisioned_checkout_helper_is_discovered_without_selecting_shared_defaults() {
  let payload = PathBuf::from(
    std::env::var_os("CTL_TEST_BUNDLED_CTLD_DIR")
      .expect("set CTL_TEST_BUNDLED_CTLD_DIR to a signed development payload directory"),
  );
  let target = crate::setup::macos::release_target().unwrap();
  let receipt = fs::read(payload.join(format!("ctld-{target}.json"))).unwrap();
  let manifest = Manifest::parse_development(&receipt, env!("CARGO_PKG_VERSION"), target).unwrap();
  let fixture = Fixture::with_bundle(
    &fs::read(payload.join(&manifest.archive)).unwrap(),
    manifest,
  );
  let candidate = fixture.load().unwrap().unwrap();
  let executable = verify_candidate(&fixture.home.0, &candidate, None)
    .await
    .unwrap()
    .unwrap();
  assert_eq!(
    executable,
    fixture
      .directory
      .canonicalize()
      .unwrap()
      .join("ctld.app/Contents/MacOS/ctld")
  );
  let managed = ctl_ipc::managed::component_directory(&fixture.home.0);
  assert!(!managed.join("selected").exists());
  assert!(!managed.join("current").exists());
}
