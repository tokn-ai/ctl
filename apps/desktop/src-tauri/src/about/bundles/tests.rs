use super::*;
use ctl_core::bundles::{Manifest, Purpose, Source, Store};
use ctl_core::component::{ComponentInfo, ProtocolInfo};
use std::collections::BTreeMap;

fn fixture(target: &str) -> (Manifest, BTreeMap<String, Vec<u8>>) {
  let protocols: BTreeMap<_, _> = [
    ctmux_proto::protocol_info(),
    ctmux_ipc::local_control_protocol_info(),
    ctl_task_proto::protocol_info(),
    ctl_task_proto::control::protocol_info(),
    ProtocolInfo::new(
      "ctl_remote_vpn",
      ctl_ipc::remote_vpn::PROTOCOL_BUILD,
      ctl_ipc::remote_vpn::PROTOCOL_VERSION,
      ctl_ipc::remote_vpn::SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ]
  .into_iter()
  .chain(ctl_proto::agent_protocols())
  .chain(ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols)
  .map(|protocol| (protocol.name.clone(), protocol))
  .collect();
  let mut build = ctl_core::component::build_info();
  build.dirty = false;
  build.source_revision = Some("a".repeat(40));
  let components = ctl_core::bundles::COMPONENTS
    .into_iter()
    .map(|name| {
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols: protocols.values().cloned().collect(),
        },
      )
    })
    .collect();
  let files = ctl_core::bundles::COMPONENTS
    .into_iter()
    .map(|name| (name.into(), name.as_bytes().to_vec()))
    .collect();
  let manifest = Manifest::new(target, Source::Release, components, &files).unwrap();
  (manifest, files)
}

#[test]
fn included_builds_are_visible_without_import_and_deduplicated_after_selection() {
  let home = std::env::temp_dir().join(format!("ctmux-included-snapshot-{}", uuid::Uuid::new_v4()));
  let (manifest, files) = fixture("aarch64-apple-darwin");
  let included = |target: &str| Ok((target == manifest.target_triple).then(|| manifest.clone()));
  let listed = snapshot_with(&home, included);
  assert_eq!(listed.errors, [] as [String; 0]);
  assert_eq!(listed.bundles.len(), 1);
  assert!(listed.bundles[0].included);
  assert!(matches!(listed.bundles[0].upload_use, BundleUse::Available));
  assert!(!home.exists(), "Opening About must stay passive");
  let store = Store::new(&home);
  let stored = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Upload, &stored).unwrap();
  let listed = snapshot_with(&home, included);
  assert_eq!(listed.bundles.len(), 1);
  assert!(listed.bundles[0].included);
  assert!(matches!(listed.bundles[0].upload_use, BundleUse::Selected));
  assert_eq!(
    store
      .selected(Purpose::Upload, &manifest.target_triple)
      .unwrap()
      .unwrap()
      .manifest,
    manifest
  );
  std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn an_included_archive_error_keeps_the_stored_selection_visible() {
  let home = std::env::temp_dir().join(format!("ctmux-included-error-{}", uuid::Uuid::new_v4()));
  let (manifest, files) = fixture("aarch64-apple-darwin");
  let store = Store::new(&home);
  let stored = store.publish(&manifest, &files).unwrap();
  store.select(Purpose::Upload, &stored).unwrap();
  let listed = snapshot_with(&home, |target| {
    if target == manifest.target_triple {
      Err(ctl_client::remote_bundle::Error::Invalid(
        "checksum mismatch".into(),
      ))
    } else {
      Ok(None)
    }
  });
  assert_eq!(listed.bundles.len(), 1);
  assert!(!listed.bundles[0].included);
  assert!(matches!(listed.bundles[0].upload_use, BundleUse::Selected));
  assert_eq!(listed.errors.len(), 1);
  assert!(listed.errors[0].contains("checksum mismatch"));
  std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn missing_or_changed_included_selections_never_create_a_store() {
  let home = std::env::temp_dir().join(format!("ctmux-included-missing-{}", uuid::Uuid::new_v4()));
  assert!(load_selection(&home, &[], "aarch64-apple-darwin", &"a".repeat(64)).is_err());
  assert!(!home.exists());
  assert!(load_selection(&home, &[], "aarch64-apple-darwin", "../unsafe").is_err());
  assert!(!home.exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_flat_included_native_archive_explains_why_local_use_is_unavailable() {
  let (manifest, _) = fixture(ctl_core::paths::native_target());
  let listed = summary(&manifest, None, None, true);
  assert!(listed.compatible);
  assert!(matches!(listed.local_use, BundleUse::Unavailable));
  assert_eq!(
    listed.local_unavailable_reason,
    Some("Requires a signed macOS helper package")
  );
  assert!(matches!(listed.upload_use, BundleUse::Available));
}

#[test]
fn absent_store_is_passive_and_selection_fields_are_snake_case() {
  let home = std::env::temp_dir().join(format!("ctmux-bundle-snapshot-{}", uuid::Uuid::new_v4()));
  let value = serde_json::to_value(snapshot(&home, &[])).unwrap();
  assert_eq!(value, serde_json::json!({"bundles": [], "errors": []}));
  assert!(!home.exists());
  let request: SelectionRequest = serde_json::from_value(serde_json::json!({
    "bundle_id": "id", "target_triple": "target", "purpose": "upload"
  }))
  .unwrap();
  assert!(matches!(request.purpose, BundlePurpose::Upload));
  assert!(
    serde_json::from_value::<SelectionRequest>(serde_json::json!({
      "bundleId": "id", "targetTriple": "target", "purpose": "upload"
    }))
    .is_err()
  );
}

fn write_included(
  directory: &std::path::Path,
  manifest: &Manifest,
  files: &BTreeMap<String, Vec<u8>>,
) {
  use sha2::{Digest as _, Sha256};
  std::fs::create_dir_all(directory).unwrap();
  let build = &manifest.components["ctl-agent"].build;
  let hashes: BTreeMap<_, _> = files
    .iter()
    .map(|(name, bytes)| (name, format!("{:x}", Sha256::digest(bytes))))
    .collect();
  let mut targets = BTreeMap::new();
  for target in [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
  ] {
    let inner = serde_json::to_vec(&serde_json::json!({
      "schema_version": 2, "app_version": build.version, "bundle_id": build.version,
      "git_revision": build.source_revision, "target_triple": target,
      "files": hashes, "components": manifest.components,
    }))
    .unwrap();
    let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
      Vec::new(),
      flate2::Compression::fast(),
    ));
    for (name, bytes) in files
      .iter()
      .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
      .chain(std::iter::once(("manifest.json", inner.as_slice())))
    {
      let mut header = tar::Header::new_ustar();
      header.set_size(bytes.len() as u64);
      header.set_mode(if name == "manifest.json" {
        0o600
      } else {
        0o700
      });
      header.set_cksum();
      archive.append_data(&mut header, name, bytes).unwrap();
    }
    let bytes = archive.into_inner().unwrap().finish().unwrap();
    let name = format!("ctl-agent-bundle-{}-{target}.tar.gz", build.version);
    std::fs::write(directory.join(&name), &bytes).unwrap();
    targets.insert(target, serde_json::json!({
      "archive": name, "sha256": format!("{:x}", Sha256::digest(bytes)), "components": manifest.components,
    }));
  }
  std::fs::write(
    directory.join("bundle-set.json"),
    serde_json::to_vec(&serde_json::json!({
      "schema_version": 2, "app_version": build.version, "bundle_id": build.version,
      "git_revision": build.source_revision, "targets": targets,
    }))
    .unwrap(),
  )
  .unwrap();
}

#[test]
fn packaged_archives_are_discovered_and_imported_only_when_explicitly_chosen() {
  let root =
    std::env::temp_dir().join(format!("ctmux-packaged-selection-{}", uuid::Uuid::new_v4()));
  let home = root.join("home");
  let directories = [root.join("resources/agent-bundles")];
  let target = "aarch64-apple-darwin";
  let (manifest, files) = fixture(target);
  write_included(&directories[0], &manifest, &files);
  let listed = snapshot(&home, &directories);
  assert!(listed.errors.is_empty(), "{:?}", listed.errors);
  assert_eq!(listed.bundles.len(), 4);
  assert!(listed.bundles.iter().all(|bundle| bundle.included));
  assert!(!home.exists());
  let id = &listed
    .bundles
    .iter()
    .find(|bundle| bundle.target_triple == target)
    .unwrap()
    .bundle_id;
  let imported = load_selection(&home, &directories, target, id).unwrap();
  assert_eq!(imported.manifest.bundle_id, *id);
  assert!(
    Store::new(&home)
      .selected(Purpose::Upload, target)
      .unwrap()
      .is_none()
  );
  Store::new(&home)
    .select(Purpose::Upload, &imported)
    .unwrap();
  let listed = snapshot(&home, &directories);
  assert_eq!(listed.bundles.len(), 4);
  assert!(matches!(
    listed
      .bundles
      .iter()
      .find(|bundle| bundle.target_triple == target)
      .unwrap()
      .upload_use,
    BundleUse::Selected
  ));
  // A broken stored selection must not be repaired silently from its app copy.
  std::fs::write(imported.directory.join("ctl-agent"), b"modified").unwrap();
  assert!(load_selection(&home, &directories, target, id).is_err());
  assert_eq!(
    std::fs::read(imported.directory.join("ctl-agent")).unwrap(),
    b"modified"
  );
  std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_changed_included_build_cannot_replace_the_one_shown_in_about() {
  let root = std::env::temp_dir().join(format!("ctmux-packaged-changed-{}", uuid::Uuid::new_v4()));
  let home = root.join("home");
  let directories = [root.join("resources/agent-bundles")];
  let target = "aarch64-apple-darwin";
  let (mut manifest, files) = fixture(target);
  write_included(&directories[0], &manifest, &files);
  let listed = snapshot(&home, &directories);
  let id = &listed
    .bundles
    .iter()
    .find(|bundle| bundle.target_triple == target)
    .unwrap()
    .bundle_id;
  for component in manifest.components.values_mut() {
    component.build.source_fingerprint = "c".repeat(64);
  }
  write_included(&directories[0], &manifest, &files);
  let error = load_selection(&home, &directories, target, id).unwrap_err();
  assert!(error.to_string().contains("changed or is unavailable"));
  assert!(!home.exists());
  std::fs::remove_dir_all(root).unwrap();
}
