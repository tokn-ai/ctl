use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
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
  select(&home.0, Purpose::Upload, &bundle).await.unwrap();
  let upload = upload_bundle(&bundle).unwrap();
  assert_eq!(
    upload.bundle_id, candidate.bundle_id,
    "older compatible agents retain their published identity"
  );
  assert_eq!(upload.git_revision, candidate.git_revision);
  assert_eq!(
    inspect_upload_archive(&upload.archive).unwrap().unwrap(),
    bundle.manifest
  );
  assert!(home.0.join(".tokn/ctl/components/bundles").is_dir());
  assert!(!home.0.join(".tokn/ctl/agent-bundles").exists());
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
  let bundle = publish_local(&home.0, components, files, &[]).unwrap();
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
async fn local_import_queries_four_components_without_starting_services_or_selecting() {
  use std::os::unix::fs::PermissionsExt as _;
  let home = Home::new();
  let candidate = Fixture::new("0.0.9", &"b".repeat(40)).bundle();
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
