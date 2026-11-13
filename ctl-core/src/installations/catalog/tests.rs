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

#[test]
fn inspection_is_passive_and_unknown_installations_keep_unknown_provenance() {
  let home = Home::new();
  let store = home.store();
  assert_eq!(store.catalog().unwrap(), Vec::<CatalogEntry>::new());
  assert!(!store.root().exists());
  let bundle = fixture(&home, crate::paths::native_target(), 1);
  let installed = store.activate(&bundle).unwrap();
  let inventory = store.catalog().unwrap();
  assert_eq!(inventory.len(), 1);
  assert_eq!(inventory[0].bundle_id, installed.manifest.bundle_id);
  assert!(inventory[0].origin.is_none());
  assert!(!exists(&store).unwrap());
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
