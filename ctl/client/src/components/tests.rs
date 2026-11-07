use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use ctl_core::bundle_layout::SERVICES as COMPONENTS;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-component-import-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&path)
      .unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[tokio::test]
async fn compatible_different_build_is_imported_without_selecting_or_running_it() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  let target = "aarch64-apple-darwin";
  let bundle = import_remote(&home.0, &candidate, target, Source::Release).unwrap();
  assert_eq!(
    bundle.manifest.components["ctl-agent"].build.version,
    "0.0.9"
  );
  let store = Store::new(&home.0);
  assert!(store.selected(Purpose::Upload, target).unwrap().is_none());
  let error = select(&home.0, Purpose::Upload, &bundle).await.unwrap_err();
  assert!(error.to_string().contains("upload selections"));
  let upload = upload_bundle(&bundle).unwrap();
  assert_eq!(
    upload.bundle_id, candidate.bundle_id,
    "older compatible agents retain their published identity"
  );
  assert_eq!(upload.git_revision, candidate.git_revision);
  assert_eq!(
    inspect_upload_archive(&mut read_archive(&upload.archive).unwrap())
      .unwrap()
      .unwrap(),
    bundle.manifest
  );
  assert!(home.0.join(".tokn/ctl/cache/components/bundles").is_dir());
  assert!(!home.0.join(".tokn/ctl/agent-bundles").exists());
}

#[test]
fn inspecting_an_included_archive_has_the_same_identity_as_importing_it() {
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  let target = "aarch64-apple-darwin";
  let inspected = inspect_remote(&candidate, target, Source::Release).unwrap();
  assert!(!home.0.join(".tokn").exists());
  let imported = import_remote(&home.0, &candidate, target, Source::Release).unwrap();
  assert_eq!(inspected, imported.manifest);
  let mut changed = candidate;
  changed.archive[0] ^= 1;
  assert!(inspect_remote(&changed, target, Source::Release).is_err());
}

#[test]
fn corrupted_artifact_does_not_publish_a_bundle() {
  let home = Home::new();
  let mut candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  candidate.archive[0] ^= 1;
  assert!(import_remote(&home.0, &candidate, "aarch64-apple-darwin", Source::Release).is_err());
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn packaging_rechecks_selected_bytes() {
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  let bundle = import_remote(&home.0, &candidate, "aarch64-apple-darwin", Source::Release).unwrap();
  std::fs::write(
    bundle.directory.join("ctl-taskd"),
    b"changed after selection",
  )
  .unwrap();
  assert!(upload_bundle(&bundle).is_err());
}

#[test]
fn local_provenance_is_complete_and_retains_a_legacy_agent_identity() {
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
  let imported = import_remote(&home.0, &candidate, "aarch64-apple-darwin", Source::Ci).unwrap();
  let mut components = imported.manifest.components;
  for component in components.values_mut() {
    component.build.dirty = true;
  }
  let files = COMPONENTS
    .into_iter()
    .map(|name| (name.into(), name.as_bytes().to_vec()))
    .collect();
  let bundle = publish_local(
    &home.0,
    ctl_core::paths::native_target(),
    components,
    files,
    &[],
  )
  .unwrap();
  assert_eq!(bundle.manifest.source, Source::Local);
  let legacy: serde_json::Value =
    serde_json::from_slice(&bundle.read_files().unwrap()["manifest.json"]).unwrap();
  assert_eq!(legacy["schema_version"], 1);
  assert_eq!(
    legacy["bundle_id"].as_str(),
    bundle.manifest.distribution_id.as_deref()
  );
  assert!(legacy.get("components").is_none());
  assert!(
    bundle
      .manifest
      .components
      .values()
      .all(|component| component.build.dirty)
  );
}

#[tokio::test]
async fn local_import_preserves_legacy_builds_and_requires_both_clients_for_new_builds() {
  use std::os::unix::fs::PermissionsExt as _;
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40))
    .with_clients()
    .bundle();
  let outer = crate::remote_bundle::BundleSet::parse_intrinsic(&candidate.manifest).unwrap();
  let components = outer
    .target("aarch64-apple-darwin")
    .unwrap()
    .components
    .as_ref()
    .unwrap();
  let source = home.0.join("build");
  std::fs::create_dir(&source).unwrap();
  for (name, info) in components {
    let script = format!(
      "#!/bin/sh\n[ \"$1\" = --component-info ] || exit 99\ncat <<'CTL_COMPONENT_INFO'\n{}\nCTL_COMPONENT_INFO\n",
      serde_json::to_string(info).unwrap()
    );
    std::fs::write(source.join(name), script).unwrap();
    std::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(0o700)).unwrap();
  }
  let bundle = import_local(&home.0, &source, None).await.unwrap();
  assert_eq!(bundle.manifest.source, Source::Local);
  assert_eq!(bundle.manifest.components, *components);
  assert_eq!(bundle.manifest.schema_version, 2);
  std::fs::remove_file(source.join("ctl")).unwrap();
  assert!(import_local(&home.0, &source, None).await.is_err());
  std::fs::remove_file(source.join("ctmux")).unwrap();
  let services = import_local(&home.0, &source, None).await.unwrap();
  assert_eq!(services.manifest.schema_version, 1);
  assert!(ctl_core::bundle_layout::Layout::Services.matches(&services.manifest.components));
  assert!(
    Store::new(&home.0)
      .selected(Purpose::Local, ctl_core::paths::native_target())
      .unwrap()
      .is_none()
  );
  assert!(
    Store::new(&home.0)
      .selected(Purpose::Upload, ctl_core::paths::native_target())
      .unwrap()
      .is_none()
  );
}

#[test]
fn signed_helper_receipts_support_checkpoints_and_installed_packages_without_ambiguity() {
  let home = Home::new();
  assert_eq!(
    helper_receipt(&home.0).unwrap_err().kind(),
    io::ErrorKind::NotFound
  );
  let installation = home.0.join("installation.json");
  let checkpoint = home.0.join("ctld-package.json");
  let compact = br#"{"schema_version":1,"component":"ctld"}"#;
  std::fs::write(&checkpoint, compact).unwrap();
  assert_eq!(helper_receipt(&home.0).unwrap(), compact);
  let formatted = b"{\n  \"component\": \"ctld\", \"schema_version\": 1\n}\n";
  std::fs::write(&installation, formatted).unwrap();
  assert_eq!(helper_receipt(&home.0).unwrap(), formatted);
  std::fs::write(&checkpoint, br#"{"schema_version":2,"component":"ctld"}"#).unwrap();
  assert!(
    helper_receipt(&home.0)
      .unwrap_err()
      .to_string()
      .contains("conflicting")
  );
  std::fs::remove_file(&checkpoint).unwrap();
  assert_eq!(helper_receipt(&home.0).unwrap(), formatted);
}

#[test]
fn a_second_unsafe_or_oversized_helper_receipt_never_becomes_a_silent_fallback() {
  let home = Home::new();
  let installation = home.0.join("installation.json");
  let checkpoint = home.0.join("ctld-package.json");
  std::fs::write(&installation, b"{}").unwrap();
  std::os::unix::fs::symlink(&installation, &checkpoint).unwrap();
  assert!(helper_receipt(&home.0).is_err());
  std::fs::remove_file(&checkpoint).unwrap();
  std::fs::write(
    &checkpoint,
    vec![b' '; ctl_core::bundles::MAX_MANIFEST_BYTES + 1],
  )
  .unwrap();
  assert!(helper_receipt(&home.0).is_err());
  std::fs::write(&checkpoint, b"invalid json").unwrap();
  assert!(helper_receipt(&home.0).is_err());
}

#[tokio::test]
async fn explicit_foreign_local_build_targets_are_rejected_before_any_probe() {
  let home = Home::new();
  let target = if ctl_core::paths::native_target().starts_with("aarch64-") {
    "x86_64-unknown-linux-musl"
  } else {
    "aarch64-unknown-linux-musl"
  };
  let error = import_local_for_target(&home.0, &home.0.join("missing-binaries"), None, target)
    .await
    .unwrap_err();
  assert!(
    error
      .to_string()
      .contains("target must run on this computer")
  );
  assert!(!home.0.join(".tokn").exists());
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[tokio::test]
async fn an_explicit_portable_build_keeps_its_musl_identity_on_a_gnu_host() {
  use std::os::unix::fs::PermissionsExt as _;
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let home = Home::new();
  let fixture = Fixture::new("0.1.2", &"b".repeat(40)).with_clients();
  let components: BTreeMap<String, ComponentInfo> =
    serde_json::from_value(fixture.inner["components"].clone()).unwrap();
  let source = home.0.join("native-build");
  std::fs::create_dir(&source).unwrap();
  for (name, info) in &components {
    let script = format!(
      "#!/bin/sh\nprintf '%s\\n' '{}'\n",
      serde_json::to_string(info).unwrap()
    );
    std::fs::write(source.join(name), script).unwrap();
    std::fs::set_permissions(source.join(name), std::fs::Permissions::from_mode(0o700)).unwrap();
  }
  let target = format!(
    "{}-musl",
    ctl_core::paths::native_target()
      .strip_suffix("-gnu")
      .unwrap()
  );
  let bundle = import_local_for_target(&home.0, &source, None, &target)
    .await
    .unwrap();
  assert_eq!(bundle.manifest.target_triple, target);
  let provenance: serde_json::Value =
    serde_json::from_slice(&bundle.read_files().unwrap()["manifest.json"]).unwrap();
  assert_eq!(provenance["target_triple"], target);
  assert!(!home.0.join(".tokn/ctl/current").exists());
}
