use super::*;
use crate::remote_bundle::compatibility::tests::Fixture;
use crate::remote_bundle::published::{UpdaterInfo, VerifiedUpdater};
use ctl_core::bundles::{Manifest, Source, Store};
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-cohort-{}", uuid::Uuid::new_v4()));
    ctl_core::private_fs::create_directory(&path).unwrap();
    Self(path)
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn prepared(home: &Home, target: &str, revision: char, publication: &str) -> PreparedUpdate {
  let snapshot = PreparationSnapshot::capture(&home.0).unwrap();
  let revision = revision.to_string().repeat(40);
  let fixture = Fixture::new("0.1.0", &revision).with_clients();
  let components = serde_json::from_value(fixture.inner["components"].clone()).unwrap();
  let distribution = format!("0.1.0-dev.{}", &revision[..12]);
  let manifest = Manifest::new(target, Source::Ci, components, &fixture.files)
    .unwrap()
    .with_distribution_id(&distribution)
    .unwrap();
  let bundle = Store::cache(&home.0)
    .publish(&manifest, &fixture.files)
    .unwrap();
  let bytes = b"foreign updater fixture".to_vec();
  let info = UpdaterInfo {
    schema_version: 1,
    app_version: "0.1.0".into(),
    bundle_id: distribution.clone(),
    git_revision: revision,
    target_triple: target.into(),
    binary: format!("ctlup-{distribution}-{target}"),
    sha256: format!("{:x}", Sha256::digest(&bytes)),
    size: bytes.len() as u64,
  };
  let source = BuildSource::Provided {
    path: bundle.directory.clone(),
    local_build: false,
    ctld_package: None,
  };
  PreparedUpdate {
    bundle,
    source,
    origin: Some(CatalogOrigin {
      channel: "provided".into(),
      publication_id: Some(publication.into()),
      built_at: Some("2026-01-02T03:04:05Z".into()),
    }),
    updater: Some(bytes),
    updater_info: Some(info),
    remote_archive: None,
    snapshot,
  }
}

#[tokio::test]
async fn adding_a_target_keeps_same_publication_siblings_without_executing_foreign_code() {
  let home = Home::new();
  let first = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  let first_id = first.bundle.manifest.bundle_id.clone();
  install_prepared_components(&home.0, vec![first], None, false)
    .await
    .unwrap();
  let second = prepared(&home, "x86_64-unknown-linux-musl", 'a', "one");
  let result = install_prepared_components(&home.0, vec![second], None, false)
    .await
    .unwrap();
  assert_eq!(result.len(), 2);
  let store = Installations::new(&home.0);
  assert!(store.active().unwrap().is_none());
  assert!(store.previous().unwrap().is_none());
  assert_eq!(
    store
      .catalog()
      .unwrap()
      .iter()
      .filter(|entry| entry.current)
      .count(),
    2
  );
  assert!(store.get(&first_id).is_ok());
  assert!(store.catalog().unwrap().iter().all(|entry| {
    entry.origin.as_ref().unwrap().built_at.as_deref() == Some("2026-01-02T03:04:05Z")
  }));
}

#[tokio::test]
async fn a_complete_new_cohort_replaces_all_targets_and_removes_old_payloads() {
  let home = Home::new();
  let targets = ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"];
  let old = targets.map(|target| prepared(&home, target, 'a', "one"));
  let old_ids = old
    .iter()
    .map(|component| component.bundle.manifest.bundle_id.clone())
    .collect::<Vec<_>>();
  install_prepared_components(&home.0, old.into(), None, false)
    .await
    .unwrap();
  let fresh = targets.map(|target| prepared(&home, target, 'b', "two"));
  install_prepared_components(&home.0, fresh.into(), None, false)
    .await
    .unwrap();
  let store = Installations::new(&home.0);
  let catalog = store.catalog().unwrap();
  assert_eq!(catalog.len(), 2);
  assert!(catalog.iter().all(|entry| entry.current
    && entry.origin.as_ref().unwrap().publication_id.as_deref() == Some("two")));
  assert!(old_ids.iter().all(|id| store.get(id).is_err()));
}

#[tokio::test]
async fn missing_target_in_a_new_offline_source_preserves_all_installed_heads() {
  let home = Home::new();
  let old = ["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"]
    .map(|target| prepared(&home, target, 'a', "one"));
  install_prepared_components(&home.0, old.into(), None, false)
    .await
    .unwrap();
  let store = Installations::new(&home.0);
  let before = store.catalog().unwrap();
  let fresh = prepared(&home, "aarch64-unknown-linux-musl", 'b', "two");
  assert!(
    install_prepared_components(&home.0, vec![fresh], None, false)
      .await
      .is_err()
  );
  assert_eq!(store.catalog().unwrap(), before);
}

#[tokio::test]
async fn mixed_builds_and_false_override_publications_fail_before_catalog_commit() {
  let home = Home::new();
  let mixed = vec![
    prepared(&home, "aarch64-unknown-linux-musl", 'a', "one"),
    prepared(&home, "x86_64-unknown-linux-musl", 'b', "two"),
  ];
  assert!(
    install_prepared_components(&home.0, mixed, None, false)
      .await
      .is_err()
  );
  let candidate = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  let mut false_origin = candidate.origin.clone().unwrap();
  false_origin.channel = "pr-123".into();
  false_origin.publication_id = Some("another".into());
  assert!(
    install_prepared_components(&home.0, vec![candidate], Some(false_origin), false)
      .await
      .is_err()
  );
  assert_eq!(
    Installations::new(&home.0).catalog().unwrap(),
    Vec::<CatalogEntry>::new()
  );
}

#[tokio::test]
async fn pr_channel_override_preserves_the_verified_publication_and_date() {
  let home = Home::new();
  let candidate = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  let mut requested = candidate.origin.clone().unwrap();
  requested.channel = "pr-123".into();
  install_prepared_components(&home.0, vec![candidate], Some(requested.clone()), false)
    .await
    .unwrap();
  assert_eq!(
    Installations::new(&home.0).catalog().unwrap()[0]
      .origin
      .as_ref(),
    Some(&requested)
  );
}

#[tokio::test]
async fn wrong_updater_target_is_rejected_before_catalog_commit() {
  let home = Home::new();
  let mut candidate = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  candidate.updater_info.as_mut().unwrap().target_triple = "x86_64-unknown-linux-musl".into();
  assert!(
    install_prepared_components(&home.0, vec![candidate], None, false)
      .await
      .is_err()
  );
  assert_eq!(
    Installations::new(&home.0).catalog().unwrap(),
    Vec::<CatalogEntry>::new()
  );
}

#[tokio::test]
async fn a_slow_primary_preparation_cannot_overwrite_a_concurrently_installed_publication() {
  let home = Home::new();
  let slow = prepared(&home, "aarch64-unknown-linux-musl", 'a', "older");
  let newer = prepared(&home, "aarch64-unknown-linux-musl", 'b', "newer");
  install_prepared_components(&home.0, vec![newer], None, false)
    .await
    .unwrap();
  let store = Installations::new(&home.0);
  let before = store.catalog().unwrap();
  let error = install_prepared_components(&home.0, vec![slow], None, false)
    .await
    .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
  assert_eq!(store.catalog().unwrap(), before);
  assert_eq!(
    before[0].origin.as_ref().unwrap().publication_id.as_deref(),
    Some("newer")
  );
}

#[tokio::test]
async fn prepared_updater_identity_survives_cache_eviction_before_installation() {
  let home = Home::new();
  let candidate = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  let updater = VerifiedUpdater {
    info: candidate.updater_info.clone().unwrap(),
    bytes: candidate.updater.clone().unwrap(),
  };
  let root = home.0.join(".tokn/ctl/cache/updaters");
  crate::remote_bundle::published::cache_verified_for_manifest(
    &root,
    &candidate.bundle.manifest,
    &updater,
  )
  .unwrap();
  crate::remote_bundle::published::prune_updater_cache(&root, 0).unwrap();
  assert!(
    crate::remote_bundle::published::read_cached_updater(&root, &updater.info)
      .unwrap()
      .is_none()
  );
  install_prepared_components(&home.0, vec![candidate], None, false)
    .await
    .unwrap();
  assert_eq!(
    crate::remote_bundle::published::read_cached_updater(&root, &updater.info)
      .unwrap()
      .unwrap()
      .bytes,
    updater.bytes
  );
}

#[tokio::test]
async fn offline_source_keeps_its_verified_updater_when_the_import_cache_is_evicted() {
  let home = Home::new();
  let target = "aarch64-unknown-linux-musl";
  let candidate = prepared(&home, target, 'a', "one");
  let directory = home.0.join("provided");
  ctl_core::private_fs::create_directory(&directory).unwrap();
  let archive = directory.join("bundle.tar.gz");
  std::fs::write(
    &archive,
    crate::components::package_bundle(&candidate.bundle)
      .unwrap()
      .archive,
  )
  .unwrap();
  let info = candidate.updater_info.unwrap();
  let bytes = candidate.updater.unwrap();
  std::fs::write(
    directory.join(format!("ctlup-{target}.json")),
    serde_json::to_vec(&info).unwrap(),
  )
  .unwrap();
  std::fs::write(directory.join(&info.binary), &bytes).unwrap();
  let source = BuildSource::Provided {
    path: archive,
    local_build: false,
    ctld_package: None,
  };
  let snapshot = PreparationSnapshot::capture(&home.0).unwrap();
  let (bundle, updater, origin, updater_info) = super::super::prepare_source(
    &home.0,
    target,
    ctl_core::bundles::Purpose::Upload,
    &source,
    &[],
    &|_| {},
  )
  .await
  .unwrap();
  assert_eq!(updater.as_deref(), Some(bytes.as_slice()));
  assert!(updater_info.is_some());
  let cache = home.0.join(".tokn/ctl/cache/updaters");
  crate::remote_bundle::published::prune_updater_cache(&cache, 0).unwrap();
  let component = PreparedUpdate {
    bundle,
    updater,
    origin,
    updater_info,
    source,
    snapshot,
    remote_archive: None,
  };
  install_prepared_components(&home.0, vec![component], None, false)
    .await
    .unwrap();
  assert_eq!(
    crate::remote_bundle::published::read_cached_updater(&cache, &info)
      .unwrap()
      .unwrap()
      .bytes,
    bytes
  );
}

#[test]
fn a_fresh_local_build_records_a_utc_build_time_without_changing_source_identity() {
  let home = Home::new();
  let component = prepared(&home, "aarch64-unknown-linux-musl", 'a', "one");
  let origin = super::super::local_build_origin(&component.bundle).unwrap();
  assert_eq!(origin.channel, "local");
  assert_eq!(
    origin.publication_id.as_deref(),
    Some(format!("local.{}", "a".repeat(64)).as_str())
  );
  let built = time::OffsetDateTime::parse(
    origin.built_at.as_ref().unwrap(),
    &time::format_description::well_known::Rfc3339,
  )
  .unwrap();
  assert!(
    (time::OffsetDateTime::now_utc() - built)
      .whole_seconds()
      .abs()
      < 5
  );
}
