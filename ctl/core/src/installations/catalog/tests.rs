use super::*;
use crate::bundle_layout::{ALL, executable_name, required_protocols};
use crate::bundles::{Manifest, Source, Store};
use crate::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use crate::protocol::ProtocolVersion;
use std::path::PathBuf;

struct Home(PathBuf);

impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-catalog-{}", uuid::Uuid::new_v4()));
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

fn fixture(home: &Home, target: &str, byte: u8) -> Bundle {
  fixture_with_source(home, target, byte, Source::Ci)
}

fn fixture_with_source(home: &Home, target: &str, byte: u8, source: Source) -> Bundle {
  #[cfg(windows)]
  if target == crate::paths::native_target() {
    home
      .store()
      .install_manager(b"launcher", &super::super::manager_build_fixture(), None)
      .unwrap();
  }
  let build = ComponentBuildInfo {
    version: "0.1.2".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let version = ProtocolVersion::new(1, 0, 1);
  let components = ALL
    .into_iter()
    .map(|name| {
      let protocols = required_protocols(name)
        .iter()
        .map(|&name| ProtocolInfo::new(name, 1, version, &[version]))
        .collect();
      (
        name.into(),
        ComponentInfo {
          build: build.clone(),
          protocols,
        },
      )
    })
    .collect();
  let files = ALL
    .into_iter()
    .map(|name| (executable_name(target, name).into_owned(), vec![byte]))
    .collect();
  let manifest = Manifest::new(target, source, components, &files).unwrap();
  Store::cache(&home.0).publish(&manifest, &files).unwrap()
}

fn origin(channel: &str, publication: &str) -> CatalogOrigin {
  CatalogOrigin {
    channel: channel.into(),
    publication_id: Some(publication.into()),
    built_at: Some("2026-01-02T03:04:05Z".into()),
  }
}

fn foreign() -> &'static str {
  if crate::paths::native_target() == "x86_64-pc-windows-msvc" {
    "aarch64-unknown-linux-musl"
  } else {
    "x86_64-pc-windows-msvc"
  }
}

#[cfg(target_os = "macos")]
fn helper_fixture(home: &Home, source: &Bundle, signature: u8) -> Bundle {
  let mut files = source.read_files().unwrap();
  for name in [
    "ctld",
    "ctld-package.json",
    "ctld.app/Contents/Info.plist",
    "ctld.app/Contents/MacOS/ctld",
    "ctld.app/Contents/_CodeSignature/CodeResources",
    "ctld.app/Contents/embedded.provisionprofile",
  ] {
    files.insert(name.into(), vec![signature]);
  }
  let manifest = Manifest::new(
    &source.manifest.target_triple,
    Source::Local,
    source.manifest.components.clone(),
    &files,
  )
  .unwrap()
  .with_distribution_id("development-publication")
  .unwrap();
  Store::cache(&home.0).publish(&manifest, &files).unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn renewing_a_helper_preserves_catalog_and_checkout_heads_across_retries() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let native = fixture(&home, target, 1);
  let original = helper_fixture(&home, &native, 1);
  let renewed = helper_fixture(&home, &native, 2);
  let remote = fixture(&home, foreign(), 1);
  let local = origin("local", "development-publication");
  let provided = origin("provided", "development-publication");
  store
    .install_channel(&[original.clone(), remote.clone()], &local, true)
    .unwrap();
  let checkout_one = "a".repeat(64);
  let checkout_two = "b".repeat(64);
  store
    .activate_development(&original, &local, &checkout_one, None)
    .unwrap();
  store
    .activate_development(&original, &local, &checkout_two, None)
    .unwrap();
  store.activate_with_origin(&original, &provided).unwrap();
  store
    .remove_component("provided", crate::paths::native_target())
    .unwrap();

  let original_id = &original.manifest.bundle_id;
  let renewed_id = &renewed.manifest.bundle_id;
  let before = store.catalog().unwrap();
  let (installed, _lease) = store
    .activate_renewed_helper(&renewed, original_id)
    .unwrap();
  assert_eq!(installed.manifest, renewed.manifest);
  assert_eq!(store.active().unwrap().unwrap().manifest, renewed.manifest);
  assert!(store.previous().unwrap().is_none());
  let mut expected = Vec::new();
  for mut entry in before {
    if entry.bundle_id == *original_id {
      let mut renewed_entry = entry.clone();
      renewed_entry.bundle_id.clone_from(renewed_id);
      expected.push(renewed_entry);
      entry.current = false;
    }
    expected.push(entry);
  }
  let mut expected_state = State {
    entries: expected,
    development: [
      (format!("{checkout_one}/{target}"), renewed_id.clone()),
      (format!("{checkout_two}/{target}"), renewed_id.clone()),
    ]
    .into(),
    ..State::default()
  };
  expected_state.sort_entries();
  assert_eq!(store.catalog().unwrap(), expected_state.entries);
  assert_eq!(
    read(&store).unwrap().development,
    expected_state.development
  );

  store.activate_renewed_helper(&renewed, renewed_id).unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, renewed.manifest);
  assert!(store.previous().unwrap().is_none());
  assert_eq!(store.catalog().unwrap(), expected_state.entries);
  assert_eq!(
    read(&store).unwrap().development,
    expected_state.development
  );
}

#[cfg(target_os = "macos")]
#[test]
fn renewing_a_helper_rejects_changed_runtime_and_stale_selection() {
  let home = Home::new();
  let store = home.store();
  let original = helper_fixture(&home, &fixture(&home, crate::paths::native_target(), 1), 1);
  let other = helper_fixture(&home, &fixture(&home, crate::paths::native_target(), 2), 2);
  let renewed = helper_fixture(&home, &original, 3);
  store
    .activate_with_origin(&original, &origin("local", "one"))
    .unwrap();
  let original_id = &original.manifest.bundle_id;
  let before = store.catalog().unwrap();
  let error = store
    .activate_renewed_helper(&other, original_id)
    .unwrap_err();
  assert!(
    error
      .to_string()
      .contains("outside the ctld helper package")
  );
  assert_eq!(store.catalog().unwrap(), before);
  assert_eq!(store.active().unwrap().unwrap().manifest, original.manifest);
  assert!(store.previous().unwrap().is_none());

  store
    .activate_with_origin(&other, &origin("local", "two"))
    .unwrap();
  let before = store.catalog().unwrap();
  let error = store
    .activate_renewed_helper(&renewed, original_id)
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert_eq!(store.catalog().unwrap(), before);
  assert_eq!(store.active().unwrap().unwrap().manifest, other.manifest);
  assert!(store.previous().unwrap().is_none());
  assert!(
    !store
      .root()
      .join("installs")
      .join(renewed.manifest.bundle_id)
      .exists()
  );
}

#[test]
fn inspection_is_passive_and_first_activation_records_provided_provenance() {
  let home = Home::new();
  let store = home.store();
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(!store.root().exists());
  let bundle = fixture(&home, crate::paths::native_target(), 1);
  let installed = store.activate(&bundle).unwrap();
  let inventory = store.catalog().unwrap();
  assert_eq!(inventory.len(), 1);
  assert_eq!(inventory[0].bundle_id, installed.manifest.bundle_id);
  assert_eq!(
    inventory[0].origin,
    Some(CatalogOrigin {
      channel: "provided".into(),
      publication_id: Some(installed.manifest.bundle_id),
      built_at: None,
    })
  );
  assert!(exists(&store).unwrap());
}

#[test]
fn unselected_bundles_helper_cache_and_upload_pins_do_not_create_a_registry() {
  let home = Home::new();
  let store = home.store();
  let source = fixture(&home, crate::paths::native_target(), 1);
  let old_store = Store::new(&home.0);
  let unselected = old_store
    .publish(&source.manifest, &source.read_files().unwrap())
    .unwrap();
  old_store
    .select(crate::bundles::Purpose::Upload, &unselected)
    .unwrap();
  crate::private_fs::create_directory(&old_store.root().join("ctld-helper-cache")).unwrap();
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(store.list().unwrap().is_empty());
  assert!(store.active().unwrap().is_none());
  assert!(store.previous().unwrap().is_none());
  store.activate(&source).unwrap();
  assert_eq!(store.catalog().unwrap().len(), 1);
}

#[test]
fn native_lookup_verifies_its_payload_without_reading_foreign_payload_bytes() {
  let home = Home::new();
  let store = home.store();
  let remote = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[remote], &origin("stable", "one"), false)
    .unwrap();
  let native = fixture(&home, crate::paths::native_target(), 2);
  store.activate(&native).unwrap();
  // A native lookup must stay independent of foreign payload bytes even when

  let name = installed[0].manifest.files.keys().next().unwrap();
  fs::write(
    installed[0].directory.join(name),
    b"modified foreign payload",
  )
  .unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, native.manifest);
  assert_eq!(
    store.get(&native.manifest.bundle_id).unwrap().manifest,
    native.manifest
  );
  assert!(store.get(&installed[0].manifest.bundle_id).is_err());
  assert!(store.catalog().is_err());
  assert!(store.list().is_err());
}

#[test]
fn failed_initial_activation_leaves_an_authoritative_empty_catalog_and_collectible_orphan() {
  let home = Home::new();
  let store = home.store();
  let source = fixture(&home, crate::paths::native_target(), 1);
  let result = store.activate_with(
    &source,
    None,
    None,
    |_| Ok(()),
    |_| Err::<(), _>(io::Error::other("lease preparation failed")),
  );
  assert!(
    result
      .unwrap_err()
      .to_string()
      .contains("lease preparation failed")
  );
  assert!(exists(&store).unwrap());
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(store.list().unwrap().is_empty());
  assert!(store.active().unwrap().is_none());
  assert_eq!(
    filesystem::entries(&store, &read_index(&store).unwrap())
      .unwrap()
      .len(),
    1
  );
  assert_eq!(store.prune().unwrap().removed, [source.manifest.bundle_id]);
  assert!(
    read_index(&store)
      .unwrap()
      .runtime_paths
      .locations()
      .next()
      .is_none()
  );
}

#[test]
fn readable_runtime_paths_disambiguate_builds_and_keep_the_original_source_name() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture(&home, target, 1);
  let second = fixture(&home, target, 2);
  let first = store
    .activate_with_origin(&first, &origin("stable", "one"))
    .unwrap();
  let second = store
    .activate_with_origin(&second, &origin("stable", "two"))
    .unwrap();
  let root = store.runtime_root();
  let expected = "stable-v0.1.2-aaaaaaa";
  assert_eq!(first.directory, root.join(expected).join(target));
  assert_eq!(
    second.directory,
    root
      .join(format!("{expected}-{}", &second.manifest.bundle_id[..12]))
      .join(target)
  );
  let selected = store
    .activate_with_origin(&first, &origin("nightly", "one"))
    .unwrap();
  assert_eq!(selected.directory, first.directory);
  assert_eq!(
    store.active_origin().unwrap(),
    Some(origin("nightly", "one"))
  );
  assert!(!store.updater_root().join("refs").exists());
}

#[test]
fn provisioning_refreshes_legacy_launchers_and_preserves_hash_payloads() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture(&home, target, 1);
  let second = fixture(&home, target, 2);
  store
    .install_manager(
      b"old launcher",
      &super::super::manager_build_fixture(),
      None,
    )
    .unwrap();
  let first = store.activate(&first).unwrap();
  let legacy = store.runtime_root().join(&first.manifest.bundle_id);
  fs::rename(&first.directory, &legacy).unwrap();
  let (updater, updater_lease) = store.manager_and_lease().unwrap().unwrap();
  drop(updater_lease);
  let legacy_updater = store
    .manager_root()
    .join(store.active_manager_id().unwrap().unwrap());
  fs::rename(updater.parent().unwrap(), legacy_updater).unwrap();
  fs::remove_file(store.state_root().join(RUNTIME_BUILD_PATHS)).unwrap();
  fs::remove_file(store.state_root().join(UPDATER_OBJECT_PATHS)).unwrap();
  assert_eq!(store.active().unwrap().unwrap().directory, legacy);
  let legacy_executable = legacy
    .join(executable_name(target, "ctl").as_ref())
    .canonicalize()
    .unwrap();
  let old_lease = super::super::leases::lease_executable(&legacy_executable)
    .unwrap()
    .unwrap();

  store
    .install_manager(
      b"layout 6 launcher",
      &super::super::manager_build_fixture(),
      None,
    )
    .unwrap();
  assert_eq!(
    read_index(&store)
      .unwrap()
      .updater_paths
      .object
      .launcher_layout,
    Some(super::super::DEVELOPMENT_LAYOUT_VERSION)
  );
  let launcher = store
    .root()
    .join("bin")
    .join(executable_name(target, "ctlup").as_ref());
  assert_eq!(fs::read(launcher).unwrap(), b"layout 6 launcher");
  assert_eq!(store.active().unwrap().unwrap().directory, legacy);
  let second = store.activate(&second).unwrap();
  assert_eq!(
    second
      .directory
      .file_name()
      .and_then(std::ffi::OsStr::to_str),
    Some(target)
  );
  let new_executable = second.executable("ctl").unwrap().canonicalize().unwrap();
  let _new_lease = super::super::leases::lease_executable(&new_executable)
    .unwrap()
    .unwrap();
  assert_eq!(
    store.prune().unwrap().retained_in_use,
    std::slice::from_ref(&first.manifest.bundle_id)
  );
  drop(old_lease);
  assert_eq!(store.prune().unwrap().removed, [first.manifest.bundle_id]);
  let registry: serde_json::Value =
    serde_json::from_slice(&fs::read(store.state_root().join(CATALOG)).unwrap()).unwrap();
  assert_eq!(registry["schema_version"], 2);
  assert!(registry.get("runtime_paths").is_none());
}

#[test]
fn readable_updaters_preserve_previous_layouts_and_keep_leased_exact_bytes() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let bundle = fixture(&home, target, 1);
  let build = super::super::manager_build_fixture();
  let origin = origin("stable", "fixture-release");
  let old = b"layout 4 executable";
  store.install_manager(old, &build, Some(&origin)).unwrap();
  let runtime = store.activate(&bundle).unwrap();
  let old_runtime = register_legacy_runtime(&store, &runtime);
  let original_runtime_paths = fs::read(store.state_root().join(RUNTIME_PATHS)).unwrap();
  let old_id = crate::bundles::digest(old);
  let (original_path, starting_lease) = store.manager_and_lease().unwrap().unwrap();
  drop(starting_lease);
  let old_payload = original_path.parent().unwrap();
  let label = "stable-v0.1.2-aaaaaaa";
  let legacy_group = store.manager_root().join(target);
  filesystem::ensure_directory(&legacy_group).unwrap();
  let legacy = legacy_group.join(label);
  fs::rename(old_payload, &legacy).unwrap();
  let paths = PathIndex {
    launcher_layout: Some(super::super::UPDATER_PATH_LAYOUT_VERSION),
    locations: [(old_id.clone(), format!("{target}/{label}"))].into(),
    ..PathIndex::default()
  };
  commit_updater_paths(&store, &paths).unwrap();
  fs::remove_file(store.state_root().join(UPDATER_OBJECT_PATHS)).unwrap();
  assert_eq!(
    store.manager_bytes(&old_id).unwrap().as_deref(),
    Some(old.as_slice())
  );
  let old_path = legacy
    .join(executable_name(target, "ctlup").as_ref())
    .canonicalize()
    .unwrap();
  let old_lease = super::super::leases::lease_executable(&old_path)
    .unwrap()
    .unwrap();

  let first = b"first layout 6 executable";
  let next = b"second layout 6 executable";
  store.install_manager(first, &build, Some(&origin)).unwrap();
  let first_id = crate::bundles::digest(first);
  let (path, first_lease) = store.manager_and_lease().unwrap().unwrap();
  let root = store.manager_root();
  assert_eq!(path.parent().unwrap(), root.join(label));
  store.install_manager(next, &build, Some(&origin)).unwrap();
  let next_id = crate::bundles::digest(next);
  let (next_path, _lease) = store.manager_and_lease().unwrap().unwrap();
  assert_eq!(
    next_path.parent().unwrap(),
    root.join(format!("{label}-{}", &next_id[..12]))
  );
  assert_eq!(store.active().unwrap().unwrap().directory, old_runtime);
  assert_eq!(
    fs::read(store.state_root().join(RUNTIME_PATHS)).unwrap(),
    original_runtime_paths
  );
  store.prune_unused().unwrap();
  assert_eq!(
    store.manager_bytes(&old_id).unwrap().as_deref(),
    Some(old.as_slice())
  );
  assert_eq!(
    store.manager_bytes(&first_id).unwrap().as_deref(),
    Some(first.as_slice())
  );
  drop((old_lease, first_lease));
  store.prune_unused().unwrap();
  assert!(store.manager_bytes(&old_id).unwrap().is_none());
  assert!(store.manager_bytes(&first_id).unwrap().is_none());
  assert!(!root.join(target).exists());
  assert!(!root.join(label).exists());
  assert_eq!(
    store.manager_bytes(&next_id).unwrap().as_deref(),
    Some(next.as_slice())
  );
  let named_direct = next_path.canonicalize().unwrap();
  assert!(
    super::super::leases::lease_executable(&named_direct)
      .unwrap()
      .is_some()
  );
  fs::write(&next_path, b"damaged bytes").unwrap();
  assert_eq!(
    store.manager_bytes(&next_id).unwrap_err().kind(),
    io::ErrorKind::InvalidData
  );
}

#[test]
fn flat_update_paths_respect_pending_and_leased_layout_five_reservations() {
  let home = Home::new();
  let store = home.store();
  filesystem::ensure_root(&store).unwrap();
  let target = crate::paths::native_target();
  let label = "stable-v0.1.2-aaaaaaa";
  let previous = b"layout 5 executable";
  let previous_id = crate::bundles::digest(previous);
  let paths = PathIndex {
    launcher_layout: Some(super::super::UPDATER_BUILD_PATH_LAYOUT_VERSION),
    locations: [(previous_id.clone(), format!("{label}/{target}"))].into(),
    ..PathIndex::default()
  };
  commit_updater_build_paths(&store, &paths).unwrap();
  let root = store.manager_root();
  let group = root.join(label);
  assert!(!group.exists());

  let next = b"layout 6 executable";
  let next_id = crate::bundles::digest(next);
  store
    .install_manager(
      next,
      &super::super::manager_build_fixture(),
      Some(&origin("stable", "fixture")),
    )
    .unwrap();
  let (path, _lease) = store.manager_and_lease().unwrap().unwrap();
  assert_eq!(
    path.parent().unwrap(),
    root.join(format!("{label}-{}", &next_id[..12]))
  );

  let previous_payload = group.join(target);
  filesystem::ensure_directory(&group).unwrap();
  filesystem::ensure_directory(&previous_payload).unwrap();
  let previous_path = previous_payload.join(executable_name(target, "ctlup").as_ref());
  let mut file = options()
    .write(true)
    .create_new(true)
    .open(&previous_path)
    .unwrap();
  file.write_all(previous).unwrap();
  drop(file);
  let direct = previous_path.canonicalize().unwrap();
  let legacy_lease = super::super::leases::lease_executable(&direct)
    .unwrap()
    .unwrap();
  store.prune_unused().unwrap();
  assert_eq!(
    store.manager_bytes(&previous_id).unwrap().as_deref(),
    Some(previous.as_slice())
  );
  drop(legacy_lease);
  store.prune_unused().unwrap();
  assert!(!group.exists());
  assert_eq!(
    store.manager_bytes(&next_id).unwrap().as_deref(),
    Some(next.as_slice())
  );
  assert_eq!(
    read_index(&store)
      .unwrap()
      .updater_paths
      .build_first
      .launcher_layout,
    Some(super::super::UPDATER_BUILD_PATH_LAYOUT_VERSION)
  );
}

fn register_legacy_runtime(store: &Installations, runtime: &Bundle) -> PathBuf {
  let legacy = target_first(&store.runtime_root(), &runtime.directory);
  let label = legacy.file_name().unwrap().to_str().unwrap();
  let paths = PathIndex {
    launcher_layout: Some(super::super::RUNTIME_PATH_LAYOUT_VERSION),
    locations: [(
      runtime.manifest.bundle_id.clone(),
      format!("{}/{label}", runtime.manifest.target_triple),
    )]
    .into(),
    ..PathIndex::default()
  };
  commit_runtime_paths(store, &paths).unwrap();
  fs::remove_file(store.state_root().join(RUNTIME_BUILD_PATHS)).unwrap();
  legacy
}

fn target_first(root: &Path, path: &Path) -> PathBuf {
  let build = path.parent().unwrap();
  let group = root.join(path.file_name().unwrap());
  filesystem::ensure_directory(&group).unwrap();
  let old_location = group.join(build.file_name().unwrap());
  fs::rename(path, &old_location).unwrap();
  fs::remove_dir(build).unwrap();
  old_location
}

#[test]
fn inferred_provided_origin_cannot_replace_a_foreign_channel_head() {
  let home = Home::new();
  let store = home.store();
  let remote = fixture(&home, foreign(), 1);
  store
    .install_channel(&[remote], &origin("provided", "foreign-publication"), false)
    .unwrap();
  let before = store.catalog().unwrap();
  let native = fixture(&home, crate::paths::native_target(), 2);
  let error = store.activate(&native).unwrap_err();
  assert!(
    error
      .to_string()
      .contains("complete matching channel cohort")
  );
  assert_eq!(store.catalog().unwrap(), before);
  assert!(store.active().unwrap().is_none());
  assert!(
    read_index(&store)
      .unwrap()
      .runtime_paths
      .location(&native.manifest.bundle_id)
      .is_none()
  );
}

#[test]
fn manifest_maintenance_holds_the_installation_lock_until_its_callback_returns() {
  use std::sync::mpsc;
  use std::time::Duration;

  let home = Home::new();
  let store = home.store();
  assert_eq!(
    store
      .with_locked_manifests(|manifests, missing| Ok(manifests.len() + missing.len()))
      .unwrap(),
    0
  );
  assert!(!store.root().exists());
  let first = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[first], &origin("nightly", "one"), false)
    .unwrap();
  let second = fixture(&home, foreign(), 2);
  let second_id = second.manifest.bundle_id.clone();
  let concurrent_store = store.clone();
  let (started, ready) = mpsc::channel();
  let (finished, result) = mpsc::channel();
  let mut worker = None;
  let observed = store
    .with_locked_manifests(|manifests, missing| {
      assert_eq!(manifests, std::slice::from_ref(&installed[0].manifest));
      assert_eq!(missing, &[]);
      let independent_lock = options()
        .read(true)
        .write(true)
        .open(store.state_root().join("installations.lock"))?;
      assert!(matches!(
        independent_lock.try_lock(),
        Err(fs::TryLockError::WouldBlock)
      ));
      worker = Some(std::thread::spawn(move || {
        started.send(()).unwrap();
        let installed =
          concurrent_store.install_channel(&[second], &origin("nightly", "two"), false);
        finished.send(installed).unwrap();
      }));
      ready.recv_timeout(Duration::from_secs(10)).unwrap();
      assert!(matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)));
      assert_eq!(
        store.get_recorded(&second_id).unwrap_err().kind(),
        io::ErrorKind::NotFound
      );
      Ok(manifests[0].bundle_id.clone())
    })
    .unwrap();
  assert_eq!(observed, installed[0].manifest.bundle_id);
  result
    .recv_timeout(Duration::from_secs(10))
    .unwrap()
    .unwrap();
  worker.unwrap().join().unwrap();
  assert!(store.get(&second_id).is_ok());
}

#[test]
fn foreign_channel_components_are_durable_without_becoming_native_selection() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[bundle], &origin("nightly", "one"), false)
    .unwrap();
  assert!(store.active().unwrap().is_none());
  assert!(store.previous().unwrap().is_none());
  assert!(store.activate(&installed[0]).is_err());
  assert_eq!(store.prune().unwrap().removed, Vec::<String>::new());
  assert_eq!(
    store
      .component("nightly", foreign())
      .unwrap()
      .unwrap()
      .directory,
    installed[0].directory
  );
  assert_eq!(
    store
      .get(&installed[0].manifest.bundle_id)
      .unwrap()
      .manifest
      .target_triple,
    foreign()
  );
}

#[test]
fn replacing_a_publication_updates_all_heads_and_prunes_old_unleased_targets() {
  let home = Home::new();
  let store = home.store();
  let old = fixture(&home, foreign(), 1);
  let old_id = old.manifest.bundle_id.clone();
  store
    .install_channel(&[old], &origin("nightly", "one"), false)
    .unwrap();
  let other = fixture(&home, foreign(), 2);
  let other_id = other.manifest.bundle_id.clone();
  store
    .install_channel(&[other], &origin("stable", "stable-one"), false)
    .unwrap();
  let fresh = fixture(&home, foreign(), 3);
  let fresh_id = fresh.manifest.bundle_id.clone();
  store
    .install_channel(&[fresh], &origin("nightly", "two"), false)
    .unwrap();
  assert_eq!(
    store.prune().unwrap().removed,
    std::slice::from_ref(&old_id)
  );
  assert!(store.get(&old_id).is_err());
  assert!(store.get(&other_id).is_ok());
  assert_eq!(
    store
      .component("nightly", foreign())
      .unwrap()
      .unwrap()
      .manifest
      .bundle_id,
    fresh_id
  );
  assert_eq!(store.catalog().unwrap().len(), 2);
}

#[test]
fn bulk_channel_installation_replaces_its_target_set_as_one_publication() {
  let home = Home::new();
  let store = home.store();
  let first = fixture(&home, foreign(), 1);
  let second_target = if foreign() == "aarch64-unknown-linux-musl" {
    "aarch64-apple-darwin"
  } else {
    "aarch64-unknown-linux-musl"
  };
  let second = fixture(&home, second_target, 1);
  store
    .install_channel(&[first, second], &origin("nightly", "one"), false)
    .unwrap();
  assert_eq!(
    store
      .catalog()
      .unwrap()
      .iter()
      .filter(|entry| entry.current)
      .count(),
    2
  );
  let replacement = fixture(&home, second_target, 2);
  store
    .install_channel(&[replacement], &origin("nightly", "two"), false)
    .unwrap();
  assert!(store.component("nightly", foreign()).unwrap().is_none());
  assert_eq!(store.prune().unwrap().removed.len(), 2);
  let catalog = store.catalog().unwrap();
  assert_eq!(catalog.len(), 1);
  assert_eq!(
    catalog[0]
      .origin
      .as_ref()
      .unwrap()
      .publication_id
      .as_deref(),
    Some("two")
  );
}

#[test]
fn stale_channel_preparation_cannot_remove_a_concurrently_added_target_or_activate() {
  let home = Home::new();
  let store = home.store();
  let native = fixture(&home, crate::paths::native_target(), 1);
  store
    .activate_with_origin(&native, &origin("stable", "native"))
    .unwrap();
  let first = fixture(&home, foreign(), 1);
  store
    .install_channel(
      std::slice::from_ref(&first),
      &origin("nightly", "one"),
      false,
    )
    .unwrap();
  let expected = store
    .catalog()
    .unwrap()
    .into_iter()
    .filter(|entry| entry.origin.as_ref().unwrap().channel == "nightly" && entry.current)
    .collect::<Vec<_>>();
  let prepared = fixture(&home, crate::paths::native_target(), 2);
  let second_target = if foreign() == "aarch64-unknown-linux-musl" {
    "aarch64-apple-darwin"
  } else {
    "aarch64-unknown-linux-musl"
  };
  let second = fixture(&home, second_target, 1);
  store
    .install_channel(&[first, second], &origin("nightly", "one"), false)
    .unwrap();
  let before = store.catalog().unwrap();
  let previous = store.previous().unwrap().map(|bundle| bundle.manifest);
  let error = store
    .install_channel_if_unchanged(
      std::slice::from_ref(&prepared),
      &origin("nightly", "two"),
      true,
      &expected,
    )
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert!(error.to_string().contains("refresh the catalog and retry"));
  let error = store
    .activate_and_lease_with_origin_if_unchanged(&prepared, &origin("nightly", "two"), &expected)
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert_eq!(store.catalog().unwrap(), before);
  assert_eq!(store.active().unwrap().unwrap().manifest, native.manifest);
  assert_eq!(
    store.previous().unwrap().map(|bundle| bundle.manifest),
    previous
  );
  assert_eq!(
    store.get(&prepared.manifest.bundle_id).unwrap_err().kind(),
    io::ErrorKind::NotFound
  );
  assert_eq!(
    before
      .iter()
      .filter(|entry| entry.current && entry.origin.as_ref().unwrap().channel == "nightly")
      .count(),
    2
  );
}

#[test]
fn channel_snapshot_comparison_ignores_order_and_other_channels() {
  let home = Home::new();
  let store = home.store();
  let first = fixture(&home, foreign(), 1);
  let second = fixture(&home, "x86_64-unknown-linux-musl", 1);
  store
    .install_channel(&[first, second], &origin("nightly", "one"), false)
    .unwrap();
  let mut expected = store.catalog().unwrap();
  expected.reverse();
  let stable = fixture(&home, foreign(), 3);
  store
    .install_channel(
      std::slice::from_ref(&stable),
      &origin("stable", "stable"),
      false,
    )
    .unwrap();
  let fresh = fixture(&home, foreign(), 2);
  store
    .install_channel_if_unchanged(&[fresh], &origin("nightly", "two"), false, &expected)
    .unwrap();
  assert_eq!(
    store
      .component("stable", foreign())
      .unwrap()
      .unwrap()
      .manifest,
    stable.manifest
  );
  assert!(
    store
      .component("nightly", "x86_64-unknown-linux-musl")
      .unwrap()
      .is_none()
  );
}

#[test]
fn mixed_source_builds_cannot_be_published_as_one_channel_cohort() {
  let home = Home::new();
  let store = home.store();
  let first = fixture(&home, foreign(), 1);
  let second = fixture(&home, "x86_64-unknown-linux-musl", 1);
  let files = second.read_files().unwrap();
  let mut components = second.manifest.components.clone();
  for component in components.values_mut() {
    component.build.source_revision = Some("c".repeat(40));
    component.build.source_fingerprint = "d".repeat(64);
  }
  let manifest = Manifest::new(
    &second.manifest.target_triple,
    Source::Ci,
    components,
    &files,
  )
  .unwrap();
  let second = Store::cache(&home.0).publish(&manifest, &files).unwrap();
  let ids = [
    first.manifest.bundle_id.clone(),
    second.manifest.bundle_id.clone(),
  ];
  let error = store
    .install_channel(&[first, second], &origin("nightly", "mixed"), false)
    .unwrap_err();
  assert!(error.to_string().contains("share one source build"));
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(store.active().unwrap().is_none());
  for id in ids {
    assert_eq!(store.get(&id).unwrap_err().kind(), io::ErrorKind::NotFound);
  }
}

#[test]
fn existing_activation_keeps_recorded_provenance_for_the_same_payload() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, crate::paths::native_target(), 1);
  let installed = store
    .activate_with_origin(&bundle, &origin("stable", "one"))
    .unwrap();
  let before = store.catalog().unwrap();
  store.activate(&installed).unwrap();
  assert_eq!(store.catalog().unwrap(), before);
  assert_eq!(store.prune().unwrap().removed, Vec::<String>::new());
}

#[test]
fn development_checkouts_keep_one_build_each_until_unregistered() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture_with_source(&home, target, 1, Source::Local);
  let second = fixture_with_source(&home, target, 2, Source::Local);
  let third = fixture_with_source(&home, target, 3, Source::Local);
  let stale = fixture_with_source(&home, target, 4, Source::Local);
  let checkout_one = "a".repeat(64);
  let checkout_two = "b".repeat(64);
  let origin = origin("local", "development");
  assert!(store.development_head(&checkout_one).unwrap().is_none());
  assert!(store.development_head("../other-checkout").is_err());
  let (first, lease) = store
    .activate_development(&first, &origin, &checkout_one, None)
    .unwrap();
  store
    .activate_development(&second, &origin, &checkout_two, None)
    .unwrap();
  let before = fs::read(store.state_root().join(CATALOG)).unwrap();
  let error = store
    .activate_development(&stale, &origin, &checkout_one, None)
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert_eq!(fs::read(store.state_root().join(CATALOG)).unwrap(), before);
  assert!(
    read_index(&store)
      .unwrap()
      .runtime_paths
      .location(&stale.manifest.bundle_id)
      .is_none()
  );
  assert_eq!(store.active().unwrap().unwrap().manifest, second.manifest);
  store
    .activate_development(
      &third,
      &origin,
      &checkout_one,
      Some(&first.manifest.bundle_id),
    )
    .unwrap();
  assert_eq!(
    store.development_head(&checkout_one).unwrap(),
    Some(third.manifest.bundle_id.clone())
  );
  assert_eq!(
    store.development_head(&checkout_two).unwrap(),
    Some(second.manifest.bundle_id.clone())
  );
  assert!(store.previous().unwrap().is_none());
  let catalog: serde_json::Value =
    serde_json::from_slice(&fs::read(store.state_root().join(CATALOG)).unwrap()).unwrap();
  assert_eq!(
    catalog["development"][format!("{checkout_one}/{target}")],
    third.manifest.bundle_id
  );
  assert_eq!(
    store.prune().unwrap().retained_in_use,
    std::slice::from_ref(&first.manifest.bundle_id)
  );
  drop(lease);
  assert_eq!(store.prune().unwrap().removed, [first.manifest.bundle_id]);
  assert!(store.get(&second.manifest.bundle_id).is_ok());

  assert!(store.remove_component("local", target).unwrap());
  assert!(store.development_head(&checkout_one).unwrap().is_none());
  assert!(store.development_head(&checkout_two).unwrap().is_none());
  assert_eq!(store.prune().unwrap().removed, [second.manifest.bundle_id]);
  assert_eq!(store.active().unwrap().unwrap().manifest, third.manifest);
}

#[test]
fn process_leases_keep_old_catalog_provenance() {
  let home = Home::new();
  let store = home.store();
  let first = fixture(&home, crate::paths::native_target(), 1);
  let (first, lease) = store
    .activate_and_lease_with_origin(&first, &origin("nightly", "one"))
    .unwrap();
  for (byte, publication) in [(2, "two"), (3, "three")] {
    let bundle = fixture(&home, crate::paths::native_target(), byte);
    store
      .activate_with_origin(&bundle, &origin("nightly", publication))
      .unwrap();
  }
  assert_eq!(
    store.prune().unwrap().retained_in_use,
    std::slice::from_ref(&first.manifest.bundle_id)
  );
  let inventory = store.catalog().unwrap();
  assert_eq!(inventory.len(), 2);
  assert_eq!(inventory.iter().filter(|entry| entry.current).count(), 1);
  assert!(inventory.iter().all(|entry| entry.origin.is_some()));
  let newest = store.active().unwrap().unwrap();
  assert_eq!(
    store
      .component("nightly", crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest
      .bundle_id,
    newest.manifest.bundle_id
  );
  drop(lease);
  assert_eq!(store.prune().unwrap().removed, [first.manifest.bundle_id]);
  assert_eq!(store.catalog().unwrap().len(), 1);
}

#[test]
fn removal_unregisters_a_target_while_respecting_leases_and_native_pointers() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[bundle], &origin("pr-123", "head-one"), false)
    .unwrap();
  let lease = store.lease(&installed[0]).unwrap();
  assert!(store.remove_component("pr-123", foreign()).unwrap());
  assert!(store.component("pr-123", foreign()).unwrap().is_none());
  assert!(store.get(&installed[0].manifest.bundle_id).is_ok());
  let catalog = store.catalog().unwrap();
  assert_eq!(catalog.len(), 1);
  assert_eq!(catalog[0].origin, Some(origin("pr-123", "head-one")));
  assert!(!catalog[0].current);
  assert!(!store.remove_component("pr-123", foreign()).unwrap());
  drop(lease);
  assert_eq!(
    store.prune().unwrap().removed,
    [installed[0].manifest.bundle_id.clone()]
  );
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  let native = fixture(&home, crate::paths::native_target(), 2);
  let native = store
    .activate_with_origin(&native, &origin("stable", "native"))
    .unwrap();
  assert!(
    store
      .remove_component("stable", crate::paths::native_target())
      .unwrap()
  );
  assert_eq!(store.active().unwrap().unwrap().manifest, native.manifest);
  let catalog = store.catalog().unwrap();
  assert_eq!(catalog.len(), 1);
  assert_eq!(catalog[0].origin, Some(origin("stable", "native")));
  assert!(!catalog[0].current);
  assert!(
    !store
      .remove_component("stable", crate::paths::native_target())
      .unwrap()
  );
}

#[test]
fn removal_reports_committed_registration_changes_before_deferred_cleanup() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  store
    .install_channel(&[bundle], &origin("nightly", "one"), false)
    .unwrap();
  let abandoned = store
    .staging_root()
    .join(format!("runtime-{}", uuid::Uuid::new_v4()));
  options()
    .write(true)
    .create_new(true)
    .open(&abandoned)
    .unwrap();
  assert!(store.remove_component("nightly", foreign()).unwrap());
  assert!(store.component("nightly", foreign()).unwrap().is_none());
  assert!(store.prune().is_err());
  fs::remove_file(abandoned).unwrap();
  assert_eq!(store.prune().unwrap().removed.len(), 1);
}

#[test]
fn singleton_activation_merges_targets_only_for_the_same_publication() {
  let home = Home::new();
  let store = home.store();
  let remote = fixture(&home, foreign(), 1);
  let remote_id = remote.manifest.bundle_id.clone();
  store
    .install_channel(&[remote], &origin("nightly", "one"), false)
    .unwrap();
  let native = fixture(&home, crate::paths::native_target(), 1);
  store
    .activate_with_origin(&native, &origin("nightly", "one"))
    .unwrap();
  assert_eq!(
    store
      .catalog()
      .unwrap()
      .iter()
      .filter(|entry| entry.current)
      .count(),
    2
  );
  let native = fixture(&home, crate::paths::native_target(), 2);
  store
    .activate_with_origin(&native, &origin("nightly", "two"))
    .unwrap();
  assert!(store.component("nightly", foreign()).unwrap().is_none());
  assert!(store.prune().unwrap().removed.contains(&remote_id));
}

#[cfg(unix)]
#[test]
fn invalid_provenance_and_duplicate_targets_fail_before_catalog_publication() {
  for channel in [
    "",
    "dev",
    "pr-0",
    "pr-01",
    "pr-18446744073709551616",
    "../stable",
  ] {
    assert!(origin(channel, "one").validate().is_err(), "{channel}");
  }
  for date in [
    "2026-02-29T00:00:00Z",
    "2026-01-01T24:00:00Z",
    "2026-01-01T00:00:00+00:00",
    "2026-01-01T00:00:00.Z",
  ] {
    let mut source = origin("stable", "one");
    source.built_at = Some(date.into());
    assert!(source.validate().is_err(), "{date}");
  }
  for date in ["2024-02-29T23:59:59Z", "2026-01-01T00:00:00.123456789Z"] {
    let mut source = origin("stable", "one");
    source.built_at = Some(date.into());
    source.validate().unwrap();
  }
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  assert!(
    store
      .install_channel(&[bundle.clone(), bundle], &origin("stable", "one"), false)
      .is_err()
  );
  assert!(!exists(&store).unwrap());
  assert!(store.list().unwrap().is_empty());
}

#[test]
fn missing_active_payload_does_not_block_a_verified_channel_update() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture(&home, target, 1);
  let first_origin = origin("nightly", "first");
  let installed = store.activate_with_origin(&first, &first_origin).unwrap();
  let sibling = fixture(&home, foreign(), 2);
  let sibling_origin = origin("stable", "sibling");
  let preserved = store
    .install_channel(&[sibling], &sibling_origin, false)
    .unwrap();
  fs::remove_dir_all(&installed.directory).unwrap();

  let inspection = store.inspect().unwrap();
  assert_eq!(
    inspection.active.as_deref(),
    Some(installed.manifest.bundle_id.as_str())
  );
  assert_eq!(
    inspection.missing,
    std::slice::from_ref(&installed.manifest.bundle_id)
  );
  assert_eq!(inspection.bundles[0].manifest, preserved[0].manifest);
  assert!(store.inspect_status().unwrap().bundles.is_empty());
  assert!(
    store
      .get_available(&installed.manifest.bundle_id)
      .unwrap()
      .is_none()
  );
  assert_eq!(store.active().unwrap_err().kind(), io::ErrorKind::NotFound);
  let (id, active) = store.active_selection().unwrap();
  assert_eq!(id.as_ref(), Some(&installed.manifest.bundle_id));
  assert!(active.is_none());
  let heads = store
    .catalog_index()
    .unwrap()
    .into_iter()
    .filter(|entry| entry.origin.as_ref() == Some(&first_origin))
    .collect::<Vec<_>>();
  let native = fixture(&home, target, 3);
  let next_origin = origin("nightly", "next");
  assert_eq!(
    store
      .install_channel_if_unchanged(std::slice::from_ref(&native), &next_origin, true, &[])
      .unwrap_err()
      .kind(),
    io::ErrorKind::WouldBlock
  );
  store
    .install_channel_if_unchanged(std::slice::from_ref(&native), &next_origin, true, &heads)
    .unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, native.manifest);
  assert_eq!(store.prune_unused().unwrap().removed, Vec::<String>::new());
  let inspection = store.inspect().unwrap();
  assert_eq!(
    inspection.missing,
    std::slice::from_ref(&installed.manifest.bundle_id)
  );
  assert!(inspection.entries.iter().any(|entry| {
    entry.bundle_id == installed.manifest.bundle_id
      && !entry.current
      && entry.origin.as_ref() == Some(&first_origin)
  }));
  assert!(
    inspection
      .bundles
      .iter()
      .any(|bundle| bundle.manifest == preserved[0].manifest)
  );
}

#[test]
fn a_missing_checkout_can_be_replaced_but_an_incomplete_payload_cannot() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture_with_source(&home, target, 1, Source::Local);
  let next = fixture_with_source(&home, target, 2, Source::Local);
  let origin = origin("local", "checkout");
  let checkout = "a".repeat(64);
  let (first, lease) = store
    .activate_development(&first, &origin, &checkout, None)
    .unwrap();
  drop(lease);
  fs::remove_file(first.directory.join(crate::bundles::MANIFEST_FILE)).unwrap();
  assert_eq!(
    store
      .get_available(&first.manifest.bundle_id)
      .unwrap_err()
      .kind(),
    io::ErrorKind::InvalidData
  );
  assert_eq!(
    store
      .activate_development(&next, &origin, &checkout, Some(&first.manifest.bundle_id))
      .unwrap_err()
      .kind(),
    io::ErrorKind::InvalidData
  );
  assert_eq!(
    store.inspect().unwrap_err().kind(),
    io::ErrorKind::InvalidData
  );
  fs::remove_dir_all(store.runtime_root()).unwrap();
  assert_eq!(
    store.development_head(&checkout).unwrap().as_ref(),
    Some(&first.manifest.bundle_id)
  );
  assert_eq!(
    store.inspect().unwrap().missing,
    std::slice::from_ref(&first.manifest.bundle_id)
  );
  assert!(!store.runtime_root().exists());
  let (installed, _lease) = store
    .activate_development(&next, &origin, &checkout, Some(&first.manifest.bundle_id))
    .unwrap();
  assert_eq!(installed.manifest, next.manifest);
  assert_eq!(store.active().unwrap().unwrap().manifest, next.manifest);
  assert_eq!(
    store.development_head(&checkout).unwrap().as_ref(),
    Some(&next.manifest.bundle_id)
  );
}

#[test]
fn automatic_prune_reports_missing_builds_and_removes_only_unused_present_payloads() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let first = fixture(&home, target, 1);
  let second = fixture(&home, target, 2);
  let third = fixture(&home, target, 3);
  let missing_origin = origin("nightly", "one");
  let missing = store.activate_with_origin(&first, &missing_origin).unwrap();
  let unused = store
    .activate_with_origin(&second, &origin("nightly", "two"))
    .unwrap();
  let active = store
    .activate_with_origin(&third, &origin("nightly", "three"))
    .unwrap();
  fs::remove_dir_all(&missing.directory).unwrap();

  let result = store.prune_unused().unwrap();
  assert_eq!(
    result.missing_builds,
    std::slice::from_ref(&missing.manifest.bundle_id)
  );
  assert_eq!(result.removed, [unused.manifest.bundle_id]);
  assert_eq!(result.retained_in_use, Vec::<String>::new());
  assert!(!unused.directory.exists());
  assert!(active.directory.exists());
  assert!(store.catalog_index().unwrap().iter().any(|entry| {
    entry.bundle_id == missing.manifest.bundle_id
      && entry.origin.as_ref() == Some(&missing_origin)
      && !entry.current
  }));
  store
    .with_locked_manifests(|present, absent| {
      assert_eq!(present, std::slice::from_ref(&active.manifest));
      assert_eq!(absent.len(), 1);
      assert_eq!(absent[0].target_triple, target);
      assert_eq!(absent[0].bundle_id, missing.manifest.bundle_id);
      assert_eq!(absent[0].origin.as_ref(), Some(&missing_origin));
      Ok(())
    })
    .unwrap();

  let installs = store.runtime_root();
  fs::remove_dir_all(&installs).unwrap();
  let result = store.prune_unused().unwrap();
  let mut expected = vec![missing.manifest.bundle_id, active.manifest.bundle_id];
  expected.sort();
  assert_eq!(result.missing_builds, expected);
  assert_eq!(result.removed, Vec::<String>::new());
  assert!(!installs.exists());
}

#[test]
fn explicit_prune_forgets_missing_selections_and_checkout_pins() {
  let home = Home::new();
  let store = home.store();
  let target = crate::paths::native_target();
  let history = fixture(&home, target, 1);
  let active = fixture_with_source(&home, target, 2, Source::Local);
  let retained = fixture(&home, foreign(), 3);
  let history = store
    .activate_with_origin(&history, &origin("nightly", "one"))
    .unwrap();
  let retained = store
    .install_channel(&[retained], &origin("stable", "one"), false)
    .unwrap();
  let checkout = "a".repeat(64);
  let (active, lease) = store
    .activate_development(&active, &origin("local", "dev"), &checkout, None)
    .unwrap();
  drop(lease);
  fs::remove_dir_all(&history.directory).unwrap();
  fs::remove_dir_all(&active.directory).unwrap();

  let result = store.prune().unwrap();
  let mut forgotten = vec![history.manifest.bundle_id, active.manifest.bundle_id];
  forgotten.sort();
  assert_eq!(result.forgotten_missing_builds, forgotten);
  assert_eq!(result.missing_builds, Vec::<String>::new());
  assert!(store.active().unwrap().is_none());
  assert!(store.development_head(&checkout).unwrap().is_none());
  let remaining = store.catalog().unwrap();
  assert_eq!(remaining.len(), 1);
  assert_eq!(remaining[0].bundle_id, retained[0].manifest.bundle_id);
  assert!(remaining[0].current);
  assert_eq!(store.prune().unwrap(), super::super::PruneResult::default());
}

#[test]
fn catalog_corruption_stops_inspection_and_pruning_without_deleting_payloads() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[bundle], &origin("stable", "one"), false)
    .unwrap();
  let mut state = read(&store).unwrap();
  state.entries[0].target_triple = "aarch64-apple-darwin".into();
  replace(&store.state_root().join(CATALOG), &state).unwrap();
  assert!(store.catalog().is_err());
  assert!(store.prune().is_err());
  assert!(installed[0].directory.exists());
}

#[test]
fn recorded_source_is_exact_for_shared_content_and_survives_a_missing_payload() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, crate::paths::native_target(), 1);
  let stable = origin("stable", "0.1.2");
  let nightly = origin("nightly", "same-build");
  let installed = store.activate_with_origin(&bundle, &stable).unwrap();
  store.activate_with_origin(&bundle, &nightly).unwrap();
  assert_eq!(store.active_origin().unwrap(), Some(nightly.clone()));
  store.activate_with_origin(&bundle, &stable).unwrap();
  fs::remove_dir_all(installed.directory).unwrap();
  let inspection = store.inspect_status().unwrap();
  assert_eq!(inspection.active_origin, Some(stable));
  assert_eq!(inspection.missing, [bundle.manifest.bundle_id]);
  store.prune().unwrap();
  assert!(store.active_origin().unwrap().is_none());
}

#[test]
fn a_missing_registry_cannot_be_reconstructed_from_unregistered_payloads() {
  let home = Home::new();
  let store = home.store();
  let old = fixture(&home, crate::paths::native_target(), 1);
  let installed = store.activate(&old).unwrap();
  fs::remove_file(store.state_root().join(CATALOG)).unwrap();
  assert!(store.prepare_installation().is_err());
  assert!(store.prune().is_err());
  assert!(installed.directory.exists());
}
