use super::super::tests::{REVISION, TARGET, build, bundle_manifest};
use super::super::{TemporaryDirectory, matching_bundle_from};
use super::*;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::cell::Cell;

fn protocol(name: &str, build: u16) -> Value {
  let version = format!("1.0.{build}");
  json!({"name": name, "build": build, "version": version, "supported_versions": [version]})
}

fn fixture(incompatible: bool) -> VerifiedBundle {
  let component = |protocols: Vec<Value>| json!({"build": build(), "protocols": protocols});
  let mut components = json!({
    "ctl-agent": component(vec![
      protocol("ctl_identity", 3), protocol("ctl_maintenance", 2),
      protocol("ctmux", 13), protocol("ctmux_control", 1),
      protocol("task", 4), protocol("task_control", 2),
    ]),
    "ctmuxd": component(vec![protocol("ctmux", 13), protocol("ctmux_control", 1)]),
    "ctl-taskd": component(vec![
      protocol("task", 4), protocol("task_control", 2),
      protocol("ctmux", 13), protocol("ctmux_control", 1),
    ]),
  });
  if incompatible {
    components["ctl-agent"]["protocols"][0] = json!({
      "name": "ctl_identity", "build": 3, "version": "2.0.3", "supported_versions": ["2.0.3"],
    });
  }
  let payloads = [
    ("ctl-agent", b"agent".as_slice()),
    ("ctmuxd", b"ctmux".as_slice()),
    ("ctl-taskd", b"task".as_slice()),
  ];
  let files: serde_json::Map<String, Value> = payloads
    .iter()
    .map(|(name, bytes)| {
      (
        (*name).into(),
        json!(format!("{:x}", Sha256::digest(bytes))),
      )
    })
    .collect();
  let manifest = json!({
    "schema_version": 2, "app_version": build().version, "bundle_id": build().version,
    "git_revision": REVISION, "target_triple": TARGET, "files": files, "components": components,
  });
  let metadata = serde_json::to_vec(&manifest).unwrap();
  let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
  let mut archive = tar::Builder::new(gzip);
  for (name, bytes) in payloads
    .into_iter()
    .chain([("manifest.json", metadata.as_slice())])
  {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(if name == "manifest.json" {
      0o600
    } else {
      0o755
    });
    header.set_cksum();
    archive.append_data(&mut header, name, bytes).unwrap();
  }
  let archive = archive.into_inner().unwrap().finish().unwrap();
  let mut set = bundle_manifest(&archive);
  set["schema_version"] = json!(2);
  for target in set["targets"].as_object_mut().unwrap().values_mut() {
    target["components"] = components.clone();
  }
  let directory = TemporaryDirectory::new().unwrap();
  std::fs::write(
    directory.0.join("bundle-set.json"),
    serde_json::to_vec(&set).unwrap(),
  )
  .unwrap();
  std::fs::write(
    directory
      .0
      .join(set["targets"][TARGET]["archive"].as_str().unwrap()),
    archive,
  )
  .unwrap();
  remote_bundle::read_verified_bundle(std::slice::from_ref(&directory.0), TARGET, &build())
    .unwrap()
    .unwrap()
}

fn store(root: &std::path::Path, bundle: &VerifiedBundle) {
  BundleCacheEntry::new(root, TARGET, &build())
    .unwrap()
    .store(bundle)
    .unwrap();
}

#[tokio::test]
async fn an_older_release_cache_is_reused_offline_by_a_different_source_build() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let bundle = fixture(false);
  store(&root, &bundle);
  let mut client = build();
  client.version = "0.2.0".into();
  client.source_revision = Some("b".repeat(40));
  let called = Cell::new(false);
  let reused = matching_bundle_from(TARGET, &client, vec![], false, Some(root), || {
    called.set(true);
    std::future::ready(Err(remote_bundle::Error::Download("offline".into()).into()))
  })
  .await
  .unwrap();
  assert!(!called.get());
  assert_eq!(reused.archive, bundle.archive);
  assert_eq!(reused.git_revision, REVISION);
  assert_ne!(reused.app_version, client.version);
}

#[tokio::test]
async fn development_clients_reuse_verified_cache_without_a_clean_source_identity() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let bundle = fixture(false);
  store(&root, &bundle);
  let mut client = build();
  client.dirty = true;
  client.source_revision = None;
  let reused = matching_bundle_from(TARGET, &client, vec![], false, Some(root), || async {
    panic!("a compatible cache must avoid download")
  })
  .await
  .unwrap();
  assert_eq!(reused.archive, bundle.archive);
}

#[tokio::test]
async fn a_valid_but_incompatible_cache_does_not_prevent_the_matching_download() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  store(&root, &fixture(true));
  let mut client = build();
  client.source_revision = Some("b".repeat(40));
  let called = Cell::new(false);
  let result = matching_bundle_from(TARGET, &client, vec![], false, Some(root), || {
    called.set(true);
    std::future::ready(Err(remote_bundle::Error::Download("offline".into()).into()))
  })
  .await;
  assert!(called.get());
  assert!(matches!(
    result,
    Err(Error::Bundle(remote_bundle::Error::Download(_)))
  ));
}

#[tokio::test]
async fn a_dirty_cache_miss_stops_before_downloading() {
  let directory = TemporaryDirectory::new().unwrap();
  let mut client = build();
  client.dirty = true;
  let called = Cell::new(false);
  let result = matching_bundle_from(
    TARGET,
    &client,
    vec![],
    false,
    Some(directory.0.join("cache")),
    || {
      called.set(true);
      std::future::ready(Err(remote_bundle::Error::Download("offline".into()).into()))
    },
  )
  .await;
  assert!(!called.get());
  assert!(matches!(
    result,
    Err(Error::Bundle(remote_bundle::Error::Stale(_)))
  ));
}

#[tokio::test]
async fn an_explicit_compatible_bundle_can_cross_releases_but_incompatibility_is_final() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let compatible = fixture(false);
  store(&root, &compatible);
  let override_directory = directory.0.join("override");
  std::fs::create_dir(&override_directory).unwrap();
  let mut client = build();
  client.version = "0.2.0".into();
  client.source_revision = Some("b".repeat(40));
  for incompatible in [false, true] {
    let bundle = fixture(incompatible);
    std::fs::write(override_directory.join("bundle-set.json"), &bundle.manifest).unwrap();
    std::fs::write(override_directory.join(&bundle.file_name), &bundle.archive).unwrap();
    let result = matching_bundle_from(
      TARGET,
      &client,
      vec![override_directory.clone()],
      true,
      Some(root.clone()),
      || async { panic!("an explicit bundle selection must not fall back to downloads") },
    )
    .await;
    if incompatible {
      assert!(matches!(
        result,
        Err(Error::Bundle(remote_bundle::Error::NotAvailable(_)))
      ));
    } else {
      assert_eq!(result.unwrap().archive, compatible.archive);
    }
  }
}

#[tokio::test]
async fn an_obsolete_default_manifest_does_not_block_compatible_cache_reuse() {
  let directory = TemporaryDirectory::new().unwrap();
  let root = directory.0.join("cache");
  let compatible = fixture(false);
  store(&root, &compatible);
  let resources = directory.0.join("resources");
  std::fs::create_dir(&resources).unwrap();
  // The obsolete checkout resource manifest remains after its archive was removed.
  std::fs::write(
    resources.join("bundle-set.json"),
    serde_json::to_vec(&bundle_manifest(b"obsolete archive")).unwrap(),
  )
  .unwrap();
  let mut client = build();
  client.version = "0.2.0".into();
  client.source_revision = Some("b".repeat(40));
  let reused = matching_bundle_from(
    TARGET,
    &client,
    vec![resources.clone()],
    false,
    Some(root.clone()),
    || async { panic!("the verified compatible cache should be used offline") },
  )
  .await
  .unwrap();
  assert_eq!(reused.archive, compatible.archive);
  let explicit = matching_bundle_from(
    TARGET,
    &client,
    vec![resources],
    true,
    Some(root),
    || async { panic!("an explicit obsolete selection must not fall back") },
  )
  .await;
  assert!(matches!(
    explicit,
    Err(Error::Bundle(remote_bundle::Error::NotAvailable(_)))
  ));
}
