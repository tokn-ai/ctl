use super::*;
use crate::bundle_layout::{ALL, SERVICES, executable_name, required_protocols};
#[cfg(unix)]
use crate::bundles::Purpose;
use crate::bundles::Store;
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use crate::protocol::ProtocolVersion;
use std::io::Write as _;
use std::path::{Path, PathBuf};

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-migration-{}", uuid::Uuid::new_v4()));
    crate::private_fs::create_directory(&path).unwrap();
    Self(path)
  }

  fn store(&self) -> Installations {
    Installations::new(&self.0)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn fixture(home: &Home, byte: u8, source: Source, clients: bool) -> Bundle {
  let target = crate::paths::native_target();
  #[cfg(windows)]
  home.store().install_manager(b"launcher fixture").unwrap();
  let build = ComponentBuildInfo {
    version: "0.1.3".into(),
    source_revision: Some(format!("{byte:02x}").repeat(20)),
    source_fingerprint: format!("{byte:02x}").repeat(32),
    dirty: false,
  };
  let names = if clients { &ALL[..] } else { &SERVICES[..] };
  let version = ProtocolVersion::new(1, 0, 1);
  let components = names
    .iter()
    .map(|&name| {
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols: required_protocols(name)
            .iter()
            .map(|&name| ProtocolInfo::new(name, 1, version, &[version]))
            .collect(),
        },
      )
    })
    .collect();
  let mut files = names
    .iter()
    .map(|name| (executable_name(target, name).into_owned(), vec![byte]))
    .collect::<BTreeMap<_, _>>();
  if cfg!(target_os = "macos") {
    files.insert("ctld.app/Contents/MacOS/ctld".into(), vec![byte]);
    files.insert("ctld-package.json".into(), b"fixture receipt".to_vec());
  }
  let manifest = Manifest::new(target, source, components, &files).unwrap();
  Store::cache(&home.0).publish(&manifest, &files).unwrap()
}

fn private_json(path: &Path, value: &impl Serialize) {
  let mut file = options().write(true).create_new(true).open(path).unwrap();
  file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
  file.sync_all().unwrap();
}

fn selection(store: &Installations) -> Vec<u8> {
  #[cfg(unix)]
  {
    serde_json::to_vec(&[
      fs::read_link(store.root.join("current")).unwrap(),
      fs::read_link(store.root.join("previous")).unwrap(),
    ])
    .unwrap()
  }
  #[cfg(windows)]
  fs::read(store.root.join(".activation.json")).unwrap()
}

#[test]
fn adoption_preserves_four_local_builds_native_history_and_live_leases() {
  let home = Home::new();
  let store = home.store();
  assert!(!store.is_catalog_migrated().unwrap());
  assert!(!store.root().exists());
  let bundles = [1, 2, 3, 4].map(|byte| fixture(&home, byte, Source::Local, true));
  let (first, lease) = store.activate_and_lease(&bundles[0]).unwrap();
  for bundle in &bundles[1..] {
    store.activate(bundle).unwrap();
  }
  let before_selection = selection(&store);
  let lease_directory = store.root.join("leases").join(&first.manifest.bundle_id);
  let before_leases = fs::read_dir(&lease_directory)
    .unwrap()
    .map(|entry| entry.unwrap().file_name())
    .collect::<Vec<_>>();
  let entries = store.migrate_catalog().unwrap();
  assert!(store.is_catalog_migrated().unwrap());
  assert_eq!(entries.len(), 4);
  assert_eq!(selection(&store), before_selection);
  assert_eq!(
    store.active().unwrap().unwrap().manifest,
    bundles[3].manifest
  );
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    bundles[2].manifest
  );
  assert_eq!(store.list().unwrap().len(), 4);
  assert!(super::super::leases::in_use(&store, &first.manifest.bundle_id).unwrap());
  assert_eq!(
    fs::read_dir(&lease_directory)
      .unwrap()
      .map(|entry| entry.unwrap().file_name())
      .collect::<Vec<_>>(),
    before_leases
  );
  for entry in &entries {
    let bundle = store.get(&entry.bundle_id).unwrap();
    let origin = entry.origin.as_ref().unwrap();
    assert_eq!(origin.channel, "local");
    assert_eq!(origin.built_at, None);
    assert_eq!(
      origin.publication_id,
      Some(format!(
        "local.{}",
        bundle.manifest.components["ctl-agent"]
          .build
          .source_fingerprint
      ))
    );
    assert_eq!(
      entry.current,
      entry.bundle_id == bundles[3].manifest.bundle_id
    );
  }
  let marker = fs::read(store.root.join(MARKER)).unwrap();
  let catalog = fs::read(store.root.join("catalog.json")).unwrap();
  assert_eq!(store.migrate_catalog().unwrap(), entries);
  assert_eq!(fs::read(store.root.join(MARKER)).unwrap(), marker);
  assert_eq!(fs::read(store.root.join("catalog.json")).unwrap(), catalog);
  assert_eq!(selection(&store), before_selection);
  drop(lease);
}

#[test]
fn adoption_preserves_known_origins_and_heads_and_uses_provided_for_ambiguous_sources() {
  let home = Home::new();
  let store = home.store();
  let known = fixture(&home, 1, Source::Ci, true);
  let known_origin = CatalogOrigin {
    channel: "nightly".into(),
    publication_id: Some("verified-publication".into()),
    built_at: Some("2026-01-02T03:04:05Z".into()),
  };
  store
    .install_channel(std::slice::from_ref(&known), &known_origin, false)
    .unwrap();
  let local_head = fixture(&home, 2, Source::Local, true);
  let local_origin = CatalogOrigin {
    channel: "local".into(),
    publication_id: Some("recorded-local-publication".into()),
    built_at: Some("2026-02-03T04:05:06Z".into()),
  };
  store
    .install_channel(std::slice::from_ref(&local_head), &local_origin, false)
    .unwrap();
  let before = catalog::read(&store).unwrap().entries;
  let active = fixture(&home, 3, Source::Local, true);
  store.activate(&active).unwrap();
  let release = fixture(&home, 4, Source::Release, true);
  filesystem::publish(&store, &release, None).unwrap();
  let ci = fixture(&home, 5, Source::Ci, true);
  let manifest = ci
    .manifest
    .clone()
    .with_distribution_id("0.1.3-dev.050505050505")
    .unwrap();
  let ci = Store::cache(&home.0)
    .publish(&manifest, &ci.read_files().unwrap())
    .unwrap();
  filesystem::publish(&store, &ci, None).unwrap();
  let entries = store.migrate_catalog().unwrap();
  for entry in before {
    assert!(entries.contains(&entry));
  }
  let active_entry = entries
    .iter()
    .find(|entry| entry.bundle_id == active.manifest.bundle_id)
    .unwrap();
  assert!(
    !active_entry.current,
    "the recorded local channel head must survive"
  );
  for (bundle, publication) in [
    (&release, &release.manifest.bundle_id),
    (&ci, ci.manifest.distribution_id.as_ref().unwrap()),
  ] {
    let entry = entries
      .iter()
      .find(|entry| entry.bundle_id == bundle.manifest.bundle_id)
      .unwrap();
    assert_eq!(
      entry.origin.as_ref().unwrap(),
      &CatalogOrigin {
        channel: "provided".into(),
        publication_id: Some(publication.clone()),
        built_at: None,
      }
    );
    assert!(!entry.current);
  }
}

#[test]
fn interrupted_catalog_commit_recovers_before_publishing_the_migration_marker() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, 1, Source::Local, true);
  store.activate(&bundle).unwrap();
  let origin = CatalogOrigin {
    channel: "provided".into(),
    publication_id: Some("recorded-origin".into()),
    built_at: Some("2026-01-02T03:04:05Z".into()),
  };
  let entry = CatalogEntry {
    bundle_id: bundle.manifest.bundle_id,
    target_triple: bundle.manifest.target_triple,
    origin: Some(origin),
    current: true,
  };
  private_json(
    &store.root.join(".catalog-transaction.json"),
    &serde_json::json!({
      "catalog": {"schema_version": 1, "entries": [entry]},
      "active": null,
      "previous": null,
    }),
  );
  assert_eq!(store.migrate_catalog().unwrap(), [entry]);
  assert!(store.is_catalog_migrated().unwrap());
  assert!(!store.root.join(".catalog-transaction.json").exists());
  // Simulate a crash after catalog publication but before the final marker.
  fs::remove_file(store.root.join(MARKER)).unwrap();
  let before = store.catalog().unwrap();
  assert_eq!(store.migrate_catalog().unwrap(), before);
}

#[test]
fn authoritative_catalog_ignores_orphans_and_requires_origins_for_new_activations() {
  let home = Home::new();
  let store = home.store();
  let known = fixture(&home, 1, Source::Local, true);
  store.activate(&known).unwrap();
  let entries = store.migrate_catalog().unwrap();
  let orphan = fixture(&home, 2, Source::Local, true);
  let error = store.activate(&orphan).unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
  assert!(
    !store
      .root
      .join("installs")
      .join(&orphan.manifest.bundle_id)
      .exists()
  );
  filesystem::publish(&store, &orphan, None).unwrap();
  assert_eq!(store.catalog().unwrap(), entries);
  assert_eq!(store.list().unwrap().len(), 2);
  store.activate(&known).unwrap();
  assert_eq!(store.catalog().unwrap(), entries);
  filesystem::activate(&store, &orphan, None).unwrap();
  assert!(
    store
      .migrate_catalog()
      .unwrap_err()
      .to_string()
      .contains("uncatalogued selection")
  );
  assert_eq!(store.catalog().unwrap(), entries);
  filesystem::activate(&store, &store.get(&known.manifest.bundle_id).unwrap(), None).unwrap();
  fs::remove_file(store.root.join("catalog.json")).unwrap();
  assert!(
    store
      .is_catalog_migrated()
      .unwrap_err()
      .to_string()
      .contains("missing")
  );
  assert!(store.catalog().is_err());
}

#[cfg(unix)]
#[test]
fn complete_selected_layout_is_copied_once_and_never_rediscovered_after_cutover() {
  let home = Home::new();
  let store = home.store();
  let source = fixture(&home, 1, Source::Local, true);
  let legacy = Store::new(&home.0);
  let source = legacy
    .publish(&source.manifest, &source.read_files().unwrap())
    .unwrap();
  legacy.select(Purpose::Local, &source).unwrap();
  let entries = store.migrate_catalog().unwrap();
  assert_eq!(entries.len(), 1);
  let installed = store.active().unwrap().unwrap();
  assert_eq!(installed.manifest, source.manifest);
  assert!(installed.directory.starts_with(store.root.join("installs")));
  assert!(
    source.directory.exists(),
    "unknown old process paths must remain available"
  );
  fs::remove_file(store.root.join("current")).unwrap();
  assert!(store.active().unwrap().is_none());
  std::os::unix::fs::symlink(
    source.directory.strip_prefix(&store.root).unwrap(),
    store.root.join("current"),
  )
  .unwrap();
  assert!(
    store.active().is_err(),
    "migrated pointers must remain canonical"
  );
}

#[cfg(unix)]
#[test]
fn service_only_manifests_fail_before_catalog_or_marker_publication() {
  let home = Home::new();
  let store = home.store();
  let active = fixture(&home, 1, Source::Local, true);
  store.activate(&active).unwrap();
  let old = fixture(&home, 2, Source::Local, false);
  filesystem::publish(&store, &old, None).unwrap();
  let error = store.migrate_catalog().unwrap_err().to_string();
  assert!(error.contains("ctlup update"));
  assert!(!store.root.join(MARKER).exists());
  assert!(!store.root.join("catalog.json").exists());
  assert_eq!(store.active().unwrap().unwrap().manifest, active.manifest);
  assert!(store.get(&old.manifest.bundle_id).is_ok());
}
