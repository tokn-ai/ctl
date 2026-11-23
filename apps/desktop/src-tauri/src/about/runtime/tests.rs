use super::*;
use serde_json::json;

fn local_store(native_target: &str) -> LocalRuntimeStore<'_> {
  LocalRuntimeStore {
    native_target,
    labels: BTreeMap::new(),
  }
}

#[test]
fn store_labels_follow_both_readable_runtime_layouts() {
  let target = "aarch64-unknown-linux-musl";
  let root = Path::new("store/runtime");
  for label in [
    "stable-v0.1.5-bbbbbbb",
    "nightly-20261116T010203Z-v0.1.5-bbbbbbb-b0123456789a",
    "dev-20261116T010203Z-v0.1.5-bbbbbbb-pr-9",
    "dev-20261116T010203Z-v0.1.5-bbbbbbb-local",
  ] {
    assert_eq!(
      store_label(&root.join(label).join(target), target),
      Some(label)
    );
    assert_eq!(
      store_label(&root.join(target).join(label), target),
      Some(label)
    );
  }
  assert!(store_label(&root.join("a".repeat(64)), target).is_none());
}

#[test]
fn inventory_preserves_selected_source_and_missing_build() {
  let id = "a".repeat(64);
  let origin = json!({"channel":"stable", "publication_id":"v0.1.5"});
  let mut inventory = json!({
    "schema_version": 2, "active": id, "previous": null, "active_origin": origin,
    "installations": [{"bundle_id": id, "target_triple": "x86_64-unknown-linux-musl",
      "current": true, "origin": origin}],
    "builds": [{"bundle_id": id, "target_triple": "x86_64-unknown-linux-musl", "source": "ci",
      "build": {"version":"0.1.5", "source_revision":"b".repeat(40),
        "source_fingerprint": "c".repeat(64), "dirty":false}}],
  });
  let status = |value| {
    AboutRuntimeStatus::from_inventory(serde_json::from_value(value).unwrap(), None).unwrap()
  };
  let active = status(inventory.clone());
  let local_targets = AboutRuntimeStatus::from_inventory(
    serde_json::from_value(inventory.clone()).unwrap(),
    Some(local_store("x86_64-unknown-linux-musl")),
  )
  .unwrap()
  .installed_targets
  .unwrap();
  assert_eq!(
    local_targets[0]
      .current_publications
      .get("stable")
      .map(String::as_str),
    Some("v0.1.5")
  );
  assert_eq!(active.build_label.as_deref(), Some("stable-v0.1.5-bbbbbbb"));
  assert_eq!(active.active_build.unwrap().version, "0.1.5");
  assert_eq!(
    active.target_triple.as_deref(),
    Some("x86_64-unknown-linux-musl")
  );
  assert!(active.installed_targets.is_none());
  assert_eq!(active.origin.unwrap().channel, "stable");
  assert_eq!(active.build_channels.len(), 1);
  assert_eq!(
    active.build_channels[0].channels,
    BTreeSet::from(["stable".into()])
  );
  assert!(!active.missing);
  inventory["builds"] = json!([]);
  inventory["missing_builds"] = json!([id]);
  let local_missing: InstallationShow = serde_json::from_value(inventory.clone()).unwrap();
  assert!(
    AboutRuntimeStatus::from_inventory(
      local_missing,
      Some(local_store("x86_64-unknown-linux-musl"))
    )
    .unwrap()
    .installed_targets
    .unwrap()
    .is_empty()
  );
  let missing = status(inventory);
  assert!(missing.missing);
  assert!(missing.build_label.is_none());
  assert!(missing.active_build.is_none());
  assert_eq!(
    missing.target_triple.as_deref(),
    Some("x86_64-unknown-linux-musl")
  );
  assert!(missing.build_channels.is_empty());
  assert_eq!(missing.active, Some(id));
}

#[test]
fn fetch_receipt_requires_a_complete_verified_result_for_the_requested_target() {
  let linux = "aarch64-unknown-linux-musl";
  let windows = "x86_64-pc-windows-msvc";
  let row = |target| json!({"package":"full_bundle","bundle_id":"a".repeat(64),"target_triple":target,"services_preserved":true});
  let check = |rows| verify_fetch(&serde_json::to_vec(&rows).unwrap(), windows);
  assert!(check(json!([row(linux), row(windows)])).is_ok());
  assert!(check(json!([row(linux)])).is_err());
  assert!(check(json!([row(windows), row(windows)])).is_err());
  for (field, bad) in [
    ("package", json!("ctl_agent")),
    ("bundle_id", json!("invalid")),
    ("services_preserved", json!(false)),
  ] {
    let mut invalid = row(windows);
    invalid[field] = bad;
    assert!(check(json!([invalid])).is_err());
  }
}

const MUSL: &str = "aarch64-unknown-linux-musl";
const GNU: &str = "aarch64-unknown-linux-gnu";
const FOREIGN: &str = "x86_64-unknown-linux-musl";

fn retained_inventory() -> InstallationShow {
  let mut entries = Vec::new();
  let mut builds = Vec::new();
  for (id, target, fingerprint, channel) in [
    ("a", MUSL, "a", "nightly"),
    ("b", MUSL, "a", "provided"),
    ("c", MUSL, "b", "pr-123"),
    ("d", FOREIGN, "a", "release"),
    ("e", GNU, "c", "stable"),
  ] {
    let id = id.repeat(64);
    entries.push(
      json!({"bundle_id":id, "target_triple":target, "current":false,
      "origin":{"channel":channel}}),
    );
    builds.push(json!({"bundle_id":id, "target_triple":target, "source":"ci",
      "build":{"version":"0.1.5","source_revision":"b".repeat(40),"source_fingerprint":fingerprint.repeat(64),"dirty":false}}));
  }
  serde_json::from_value(json!({
    "schema_version":2, "active":"a".repeat(64), "active_origin":{"channel":"stable"},
    "previous":null, "installations":entries, "builds":builds,
  }))
  .unwrap()
}

#[test]
fn installed_targets_keep_each_stored_bundle_and_its_exact_memberships() {
  let first_label = "nightly-20261116T010203Z-v0.1.5-bbbbbbb";
  let second_label = format!("{first_label}-{}", "b".repeat(12));
  let mut store = local_store(MUSL);
  store.labels.insert("a".repeat(64), first_label.into());
  store.labels.insert("b".repeat(64), second_label.clone());
  let local = AboutRuntimeStatus::from_inventory(retained_inventory(), Some(store)).unwrap();
  let targets = local.installed_targets.unwrap();
  assert_eq!(targets.len(), 3);
  let native_rows: BTreeMap<_, _> = targets
    .iter()
    .find(|entry| entry.target_triple == MUSL)
    .unwrap()
    .builds
    .iter()
    .map(|row| (row.bundle_id.clone(), row))
    .collect();
  assert_eq!(native_rows.len(), 3);
  let first = native_rows[&"a".repeat(64)];
  assert_eq!(first.build_label.as_deref(), Some(first_label));
  assert_eq!(
    first.status.channels,
    BTreeSet::from(["nightly".into(), "stable".into()])
  );
  assert!(first.status.current);
  let second = native_rows[&"b".repeat(64)];
  assert_eq!(second.build_label.as_ref(), Some(&second_label));
  assert_eq!(second.status.channels, BTreeSet::from(["provided".into()]));
  assert!(!second.status.current);
  let legacy_id = "c".repeat(64);
  let legacy = native_rows[&legacy_id];
  assert!(legacy.build_label.is_none());
  assert_eq!(legacy.bundle_id, legacy_id);
  let foreign_builds = &targets
    .iter()
    .find(|entry| entry.target_triple == FOREIGN)
    .unwrap()
    .builds;
  assert_eq!(foreign_builds.len(), 1);
  assert_eq!(
    foreign_builds[0].status.channels,
    BTreeSet::from(["release".into()])
  );
}

#[test]
fn channel_memberships_use_complete_native_builds_including_retained_bundles() {
  let mut inventory = retained_inventory();
  let channels = |inventory: &InstallationShow, target: Option<&str>| {
    AboutRuntimeStatus::from_inventory(inventory.clone(), target.map(local_store))
      .unwrap()
      .build_channels
      .into_iter()
      .map(|row| (row.build.source_fingerprint, row.channels))
      .collect::<BTreeMap<_, _>>()
  };
  let expected = BTreeMap::from([
    (
      "a".repeat(64),
      BTreeSet::from(["nightly".into(), "provided".into(), "stable".into()]),
    ),
    ("b".repeat(64), BTreeSet::from(["pr-123".into()])),
  ]);
  assert_eq!(channels(&inventory, None), expected);
  let mut expected_gnu = expected.clone();
  expected_gnu.insert("c".repeat(64), BTreeSet::from(["stable".into()]));
  // A GNU host can run its architecture's musl bundles. A selected musl bundle
  // alone supplies no evidence that GNU bundles can run on that remote host.
  assert_eq!(channels(&inventory, Some(GNU)), expected_gnu);
  inventory.previous = Some("e".repeat(64));
  assert_eq!(channels(&inventory, None), expected_gnu);
  inventory.active = inventory.previous.take();
  assert_eq!(channels(&inventory, None), channels(&inventory, Some(GNU)));
  inventory.active = None;
  inventory.previous = Some("a".repeat(64));
  inventory.active_origin = None;
  assert!(
    AboutRuntimeStatus::from_inventory(inventory.clone(), None)
      .unwrap()
      .target_triple
      .is_none()
  );
  assert_eq!(channels(&inventory, None), channels(&inventory, Some(MUSL)));
  inventory.previous = None;
  assert!(channels(&inventory, None).is_empty());
  assert_eq!(channels(&inventory, Some(MUSL)).len(), 2);
}

#[test]
fn empty_local_profile_does_not_require_or_install_a_manager() {
  let home = tempfile::tempdir().unwrap();
  assert!(
    AboutRuntimeStatus::local(home.path())
      .unwrap()
      .active
      .is_none()
  );
  assert!(!home.path().join(".tokn").exists());
}

#[test]
fn sync_accepts_only_a_completed_result_for_the_selected_target() {
  let parse = |row| sync_result(&serde_json::to_vec(&json!([row])).unwrap(), "machine");
  for state in ["pinned", "current"] {
    assert_eq!(
      parse(json!({"host":"machine","check":{"state":state}}))
        .unwrap()
        .state,
      state
    );
  }
  let result = json!({"package":"full_bundle","bundle_id":"a".repeat(64),
    "target_triple":"x86_64-unknown-linux-musl","services_preserved":true});
  assert_eq!(
    parse(json!({"host":"machine","result": result}))
      .unwrap()
      .state,
    "installed"
  );
  assert!(parse(json!({"host":"other","result": result})).is_err());
  assert!(parse(json!({"host":"machine","check":{"state":"missing"}})).is_err());
  assert!(parse(json!({"host":"machine","error":"Host identity changed."})).is_err());
}

#[test]
fn freezes_only_an_exact_saved_host_and_method() {
  let mut snapshot: HostCatalogSnapshot = serde_json::from_value(json!({
    "revision":"test", "document":{"schema_version":1, "ssh_gateways":[], "hosts":[{
      "host_id":"machine", "name":"Work", "preferred_method_id":"direct",
      "connection_methods":[{"method_id":"direct", "name":"Direct route",
        "target":{"kind":"ssh", "destination":"example.invalid"}}],
      "remote_info":{"remote_id":"10788361-25df-4cc9-9863-5360603697a7","agent_version":"0.1.5"}
    }]}
  }))
  .unwrap();
  assert!(SavedTarget::bind(&snapshot, "Work".into(), None).is_err());
  assert!(SavedTarget::bind(&snapshot, "machine".into(), Some("Direct route".into())).is_err());
  let target = SavedTarget::bind(&snapshot, "machine".into(), None).unwrap();
  snapshot.document.hosts[0].remote_info = None;
  assert!(SavedTarget::bind(&snapshot, "machine".into(), None).is_err());
  let frozen = ctl_client::hosts::load_catalog(target.catalog.path()).unwrap();
  assert!(frozen.hosts[0].remote_info.is_some());
  assert_eq!(target.method, "direct");
}
