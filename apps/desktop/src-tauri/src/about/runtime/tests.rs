use super::*;
use serde_json::json;

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
  assert_eq!(active.build_label.as_deref(), Some("stable-v0.1.5-bbbbbbb"));
  assert_eq!(active.origin.unwrap().channel, "stable");
  assert_eq!(active.build_channels.len(), 1);
  assert_eq!(
    active.build_channels[0].channels,
    BTreeSet::from(["stable".into()])
  );
  assert!(!active.missing);
  inventory["builds"] = json!([]);
  inventory["missing_builds"] = json!([id]);
  let missing = status(inventory);
  assert!(missing.missing);
  assert!(missing.build_label.is_none());
  assert!(missing.build_channels.is_empty());
  assert_eq!(missing.active, Some(id));
}

#[test]
fn channel_memberships_use_complete_native_builds_including_retained_bundles() {
  let native = "aarch64-unknown-linux-musl";
  let gnu = "aarch64-unknown-linux-gnu";
  let foreign = "x86_64-unknown-linux-musl";
  let mut entries = Vec::new();
  let mut builds = Vec::new();
  for (id, target, fingerprint, channel) in [
    ("a", native, "a", "nightly"),
    ("b", native, "a", "provided"),
    ("c", native, "b", "pr-123"),
    ("d", foreign, "a", "release"),
    ("e", gnu, "c", "stable"),
  ] {
    let id = id.repeat(64);
    entries.push(
      json!({"bundle_id":id, "target_triple":target, "current":false,
      "origin":{"channel":channel}}),
    );
    builds.push(json!({"bundle_id":id, "target_triple":target, "source":"ci",
      "build":{"version":"0.1.5","source_revision":"b".repeat(40),"source_fingerprint":fingerprint.repeat(64),"dirty":false}}));
  }
  let mut inventory: InstallationShow = serde_json::from_value(json!({
    "schema_version":2, "active":"a".repeat(64), "active_origin":{"channel":"stable"},
    "previous":null, "installations":entries, "builds":builds,
  }))
  .unwrap();
  let channels = |inventory: &InstallationShow, target| {
    AboutRuntimeStatus::from_inventory(inventory.clone(), target)
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
  assert_eq!(channels(&inventory, Some(gnu)), expected_gnu);
  inventory.previous = Some("e".repeat(64));
  assert_eq!(channels(&inventory, None), expected_gnu);
  inventory.active = inventory.previous.take();
  assert_eq!(channels(&inventory, None), channels(&inventory, Some(gnu)));
  inventory.active = None;
  inventory.previous = Some("a".repeat(64));
  inventory.active_origin = None;
  assert_eq!(
    channels(&inventory, None),
    channels(&inventory, Some(native))
  );
  inventory.previous = None;
  assert!(channels(&inventory, None).is_empty());
  assert_eq!(channels(&inventory, Some(native)).len(), 2);
}

#[test]
fn empty_local_profile_does_not_require_or_install_a_manager() {
  let home = tempfile::tempdir().unwrap();
  let inventory = ctl_client::installation::show(home.path()).unwrap();
  assert!(
    AboutRuntimeStatus::from_inventory(inventory, Some(ctl_core::paths::native_target()))
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
