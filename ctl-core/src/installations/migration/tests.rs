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

#[cfg(unix)]
fn old_payload(store: &Installations, bundle: &Bundle) -> Bundle {
  let destination = store.root.join("installs").join(&bundle.manifest.bundle_id);
  filesystem::ensure_directory(&destination).unwrap();
  for (name, bytes) in bundle.read_files().unwrap() {
    let path = destination.join(&name);
    let mut parent = destination.clone();
    for part in path.parent().unwrap().strip_prefix(&destination).unwrap() {
      parent.push(part);
      filesystem::ensure_directory(&parent).unwrap();
    }
    let mut file = options().write(true).create_new(true).open(path).unwrap();
    file.write_all(&bytes).unwrap();
    if bundle.manifest.files[&name].executable {
      crate::private_fs::set_executable(&file).unwrap();
    }
    file.sync_all().unwrap();
  }
  private_json(
    &destination.join(crate::bundles::MANIFEST_FILE),
    &bundle.manifest,
  );
  Bundle::open(&destination).unwrap()
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
  fs::remove_file(store.root.join("catalog.json")).unwrap();
  assert!(
    store
      .active()
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
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
  let mut state = catalog::read(&store).unwrap();
  state
    .entries
    .retain(|entry| entry.bundle_id != active.manifest.bundle_id);
  catalog::commit(&store, &state, None).unwrap();
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
fn authoritative_catalog_ignores_orphans_and_originless_writes_record_provided() {
  let home = Home::new();
  let store = home.store();
  let known = fixture(&home, 1, Source::Local, true);
  store.activate(&known).unwrap();
  let entries = store.migrate_catalog().unwrap();
  let orphan = fixture(&home, 2, Source::Local, true);
  filesystem::publish(&store, &orphan, None).unwrap();
  assert_eq!(store.catalog().unwrap(), entries);
  assert_eq!(store.list().unwrap().len(), 1);
  assert_eq!(
    store.get(&orphan.manifest.bundle_id).unwrap_err().kind(),
    io::ErrorKind::NotFound
  );
  store.activate(&known).unwrap();
  assert_eq!(store.catalog().unwrap(), entries);
  force_selected_bundle(&store, &orphan);
  assert!(
    store
      .migrate_catalog()
      .unwrap_err()
      .to_string()
      .contains("uncatalogued")
  );
  assert_eq!(store.catalog().unwrap(), entries);
  force_selected_bundle(&store, &store.get(&known.manifest.bundle_id).unwrap());
  store.activate(&orphan).unwrap();
  let updated = store.catalog().unwrap();
  assert_eq!(updated.len(), 2);
  assert_eq!(
    updated
      .iter()
      .find(|entry| entry.bundle_id == orphan.manifest.bundle_id)
      .unwrap()
      .origin
      .as_ref()
      .unwrap()
      .channel,
    "provided"
  );
  store.activate(&known).unwrap();
  assert_eq!(store.catalog().unwrap(), updated);
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

fn force_selected_bundle(store: &Installations, bundle: &Bundle) {
  // Simulate an older writer without bypassing validation in production code.
  #[cfg(unix)]
  {
    let current = store.root.join("current");
    fs::remove_file(&current).unwrap();
    std::os::unix::fs::symlink(
      Path::new("installs").join(&bundle.manifest.bundle_id),
      current,
    )
    .unwrap();
  }
  #[cfg(windows)]
  {
    let mut file = crate::private_fs::options()
      .write(true)
      .truncate(true)
      .open(store.root.join(".activation.json"))
      .unwrap();
    file
      .write_all(
        &serde_json::to_vec(&serde_json::json!({
          "active": bundle.manifest.bundle_id,
          "previous": null,
        }))
        .unwrap(),
      )
      .unwrap();
  }
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
  assert!(
    store
      .active()
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
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
  let old = old_payload(&store, &old);
  fs::remove_file(store.root.join("catalog.json")).unwrap();
  let before = fs::read_link(store.root.join("current")).unwrap();
  let error = store.migrate_catalog().unwrap_err().to_string();
  assert!(error.contains("clean reinstall"));
  assert!(!store.root.join(MARKER).exists());
  assert!(!store.root.join("catalog.json").exists());
  assert_eq!(fs::read_link(store.root.join("current")).unwrap(), before);
  assert_eq!(
    legacy::active(&store).unwrap().unwrap().manifest,
    active.manifest
  );
  assert!(Bundle::open(&old.directory).is_ok());
}

#[cfg(unix)]
#[test]
fn legacy_current_import_preserves_its_existing_complete_rollback() {
  let home = Home::new();
  let store = home.store();
  let rollback = fixture(&home, 1, Source::Local, true);
  store.activate(&rollback).unwrap();
  store
    .activate(&fixture(&home, 2, Source::Local, true))
    .unwrap();
  let source = fixture(&home, 3, Source::Local, true);
  let legacy_store = Store::new(&home.0);
  let source = legacy_store
    .publish(&source.manifest, &source.read_files().unwrap())
    .unwrap();
  fs::remove_file(store.root.join("catalog.json")).unwrap();
  fs::remove_file(store.root.join("current")).unwrap();
  std::os::unix::fs::symlink(
    source.directory.strip_prefix(&store.root).unwrap(),
    store.root.join("current"),
  )
  .unwrap();
  assert!(
    store
      .active()
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
  store.migrate_catalog().unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, source.manifest);
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    rollback.manifest
  );
  assert!(source.directory.exists());
}

#[cfg(unix)]
#[test]
fn old_agent_readers_are_confined_to_migration_and_require_owned_namespaces() {
  use std::os::unix::fs::symlink;
  let home = Home::new();
  let store = home.store();
  filesystem::ensure_root(&store).unwrap();
  let relative = Path::new("components/agents")
    .join(crate::paths::native_target())
    .join(format!("{}-legacy", "a".repeat(64)));
  let mut parent = store.root.clone();
  for part in &relative {
    parent.push(part);
    filesystem::ensure_directory(&parent).unwrap();
  }
  let old = store.root.join(&relative);
  let absolute = store.root.canonicalize().unwrap().join(&relative);
  symlink(&absolute, store.root.join("current")).unwrap();
  assert!(
    store
      .active()
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
  assert!(legacy::active(&store).unwrap().is_none());
  assert!(
    store
      .migrate_catalog()
      .unwrap_err()
      .to_string()
      .contains("clean reinstall")
  );
  assert!(!store.root.join(MARKER).exists());
  fs::remove_file(store.root.join("current")).unwrap();
  symlink(&absolute, store.root.join("previous")).unwrap();
  assert!(legacy::previous(&store).is_err());
  fs::remove_file(store.root.join("previous")).unwrap();
  let target = old.parent().unwrap();
  let relocated = target.with_file_name("relocated-target");
  fs::rename(target, &relocated).unwrap();
  symlink(&relocated, target).unwrap();
  symlink(&absolute, store.root.join("current")).unwrap();
  assert!(legacy::active(&store).is_err());
}

#[cfg(unix)]
#[test]
fn unmanaged_version_payloads_require_replacement_before_migration() {
  let home = Home::new();
  let store = home.store();
  filesystem::ensure_root(&store).unwrap();
  filesystem::ensure_directory(&store.root.join("versions")).unwrap();
  let old = store.root.join("versions/old-build");
  filesystem::ensure_directory(&old).unwrap();
  let mut file = options()
    .write(true)
    .create_new(true)
    .open(old.join("ctl-agent"))
    .unwrap();
  file.write_all(b"old unmanaged agent").unwrap();
  drop(file);
  std::os::unix::fs::symlink("versions/old-build", store.root.join("current")).unwrap();
  assert!(
    store
      .active()
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
  assert!(
    store
      .migrate_catalog()
      .unwrap_err()
      .to_string()
      .contains("clean reinstall")
  );
  assert_eq!(
    fs::read(old.join("ctl-agent")).unwrap(),
    b"old unmanaged agent"
  );
  assert!(!store.root.join(MARKER).exists());
}
