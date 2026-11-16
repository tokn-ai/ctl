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
  #[cfg(windows)]
  if target == crate::paths::native_target() {
    home.store().install_manager(b"launcher").unwrap();
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
  let manifest = Manifest::new(target, Source::Ci, components, &files).unwrap();
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
fn renewing_a_helper_preserves_catalog_and_rollback_across_retries() {
  let home = Home::new();
  let store = home.store();
  let native = fixture(&home, crate::paths::native_target(), 1);
  let original = helper_fixture(&home, &native, 1);
  let renewed = helper_fixture(&home, &native, 2);
  let remote = fixture(&home, foreign(), 1);
  let local = origin("local", "development-publication");
  let provided = origin("provided", "development-publication");
  store
    .install_channel(&[original.clone(), remote.clone()], &local, true)
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
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    original.manifest
  );
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
    schema_version: 1,
    entries: expected,
  };
  expected_state.sort_entries();
  assert_eq!(store.catalog().unwrap(), expected_state.entries);

  store.activate_renewed_helper(&renewed, renewed_id).unwrap();
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    original.manifest
  );
  store.rollback().unwrap();
  store
    .activate_renewed_helper(&renewed, original_id)
    .unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, renewed.manifest);
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    original.manifest
  );
  assert_eq!(store.catalog().unwrap(), expected_state.entries);
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
  assert_eq!(
    store.previous().unwrap().unwrap().manifest,
    original.manifest
  );
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
fn unselected_bundles_helper_cache_and_upload_pins_do_not_require_migration() {
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
  let result = store.activate_with(&source, None, None, |_| {
    Err::<(), _>(io::Error::other("lease preparation failed"))
  });
  assert!(
    result
      .unwrap_err()
      .to_string()
      .contains("lease preparation failed")
  );
  assert!(exists(&store).unwrap());
  assert!(!store.is_catalog_migrated().unwrap());
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(store.list().unwrap().is_empty());
  assert!(store.active().unwrap().is_none());
  assert_eq!(filesystem::entries(&store).unwrap().len(), 1);
  assert_eq!(store.prune().unwrap().removed, [source.manifest.bundle_id]);

  let old_home = Home::new();
  let old_store = old_home.store();
  let old = fixture(&old_home, crate::paths::native_target(), 2);
  filesystem::ensure_root(&old_store).unwrap();
  filesystem::publish(&old_store, &old, None).unwrap();
  let new = fixture(&old_home, crate::paths::native_target(), 3);
  assert!(
    old_store
      .activate(&new)
      .unwrap_err()
      .to_string()
      .contains("ctlup migrate")
  );
  assert!(!exists(&old_store).unwrap());
  assert!(
    !old_store
      .root
      .join("installs")
      .join(new.manifest.bundle_id)
      .exists()
  );
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
    !store
      .root
      .join("installs")
      .join(&native.manifest.bundle_id)
      .exists()
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
      .with_locked_manifests(|manifests| Ok(manifests.len()))
      .unwrap(),
    0
  );
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
    .with_locked_manifests(|manifests| {
      assert_eq!(manifests, std::slice::from_ref(&installed[0].manifest));
      let independent_lock = options()
        .read(true)
        .write(true)
        .open(store.root.join("installations.lock"))?;
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
        store.get(&second_id).unwrap_err().kind(),
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
fn native_rollback_and_process_leases_keep_old_catalog_provenance() {
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
  assert_eq!(inventory.len(), 3);
  assert_eq!(inventory.iter().filter(|entry| entry.current).count(), 1);
  assert!(inventory.iter().all(|entry| entry.origin.is_some()));
  let newest = store.active().unwrap().unwrap();
  store.rollback().unwrap();
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
  assert_eq!(store.catalog().unwrap().len(), 2);
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
    .root()
    .join("installs")
    .join(format!(".partial-{}", uuid::Uuid::new_v4()));
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
  assert_eq!(store.prune().unwrap().removed, [remote_id]);
}

#[test]
fn interrupted_catalog_and_activation_recover_together_before_pruning() {
  let home = Home::new();
  let store = home.store();
  let old = fixture(&home, crate::paths::native_target(), 1);
  let old = store
    .activate_with_origin(&old, &origin("stable", "old"))
    .unwrap();
  let fresh = fixture(&home, crate::paths::native_target(), 2);
  let lock = filesystem::Lock::acquire(&store).unwrap();
  let fresh = filesystem::publish(&store, &fresh, None).unwrap();
  let mut state = read(&store).unwrap();
  state.register(
    std::slice::from_ref(&fresh),
    &origin("stable", "new"),
    false,
  );
  replace(
    &store.root.join(JOURNAL),
    &Transaction {
      catalog: state,
      active: Some(fresh.manifest.bundle_id.clone()),
      previous: Some(old.manifest.bundle_id.clone()),
    },
  )
  .unwrap();
  drop(lock);
  assert_eq!(
    store
      .component("stable", crate::paths::native_target())
      .unwrap()
      .unwrap()
      .manifest,
    fresh.manifest
  );
  store.prune().unwrap();
  assert_eq!(store.active().unwrap().unwrap().manifest, fresh.manifest);
  assert_eq!(store.previous().unwrap().unwrap().manifest, old.manifest);
  assert!(!store.root.join(JOURNAL).exists());
}

#[cfg(unix)]
#[test]
fn interrupted_activation_validates_all_recorded_payloads_before_changing_selection() {
  for pending_catalog in [false, true] {
    let home = Home::new();
    let store = home.store();
    let old = fixture(&home, crate::paths::native_target(), 1);
    let old = store
      .activate_with_origin(&old, &origin("stable", "old"))
      .unwrap();
    let fresh = fixture(&home, crate::paths::native_target(), 2);
    let remote = fixture(&home, foreign(), 2);
    let lock = filesystem::Lock::acquire(&store).unwrap();
    let fresh = filesystem::publish(&store, &fresh, None).unwrap();
    let remote = filesystem::publish(&store, &remote, None).unwrap();
    let mut state = read(&store).unwrap();
    state.register(
      &[fresh.clone(), remote.clone()],
      &origin("stable", "new"),
      false,
    );
    if pending_catalog {
      replace(
        &store.root.join(JOURNAL),
        &Transaction {
          catalog: state,
          active: Some(fresh.manifest.bundle_id.clone()),
          previous: Some(old.manifest.bundle_id.clone()),
        },
      )
      .unwrap();
    } else {
      commit(&store, &state, None).unwrap();
    }
    let native_journal = store.root.join(".activation.json");
    replace(
      &native_journal,
      &serde_json::json!({
        "active": fresh.manifest.bundle_id,
        "previous": old.manifest.bundle_id,
      }),
    )
    .unwrap();
    let catalog_path = store
      .root
      .join(if pending_catalog { JOURNAL } else { CATALOG });
    let catalog_intent = fs::read(&catalog_path).unwrap();
    let native_intent = fs::read(&native_journal).unwrap();
    let current = fs::read_link(store.root.join("current")).unwrap();
    let remote_file = remote
      .directory
      .join(remote.manifest.files.keys().next().unwrap());
    let original = fs::read(&remote_file).unwrap();
    fs::write(&remote_file, b"damaged foreign payload").unwrap();
    drop(lock);

    assert!(store.prune().is_err());
    assert_eq!(fs::read_link(store.root.join("current")).unwrap(), current);
    assert!(!store.root.join("previous").exists());
    assert_eq!(fs::read(&catalog_path).unwrap(), catalog_intent);
    assert_eq!(fs::read(&native_journal).unwrap(), native_intent);
    fs::write(remote_file, original).unwrap();
    store.prune().unwrap();
    assert_eq!(store.active().unwrap().unwrap().manifest, fresh.manifest);
    assert_eq!(store.previous().unwrap().unwrap().manifest, old.manifest);
    assert!(!native_journal.exists());
    assert!(!store.root.join(JOURNAL).exists());
  }
}

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
fn catalog_corruption_stops_inspection_and_pruning_without_deleting_payloads() {
  let home = Home::new();
  let store = home.store();
  let bundle = fixture(&home, foreign(), 1);
  let installed = store
    .install_channel(&[bundle], &origin("stable", "one"), false)
    .unwrap();
  let mut state = read(&store).unwrap();
  state.entries[0].target_triple = "aarch64-apple-darwin".into();
  replace(&store.root.join(CATALOG), &state).unwrap();
  assert!(store.catalog().is_err());
  assert!(store.prune().is_err());
  assert!(installed[0].directory.exists());
}
