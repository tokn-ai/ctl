//! Compatibility with a remote ctlup whose status response contains manifests.

use super::*;
use ctl_core::bundles::{Manifest, Source};
use serde_json::Value;
use std::{path::Path, process::Output};

const LEGACY_MANAGER: &str = r#"
printf '%s\n' "$*" >> "$CTL_TEST_DIR/manager-calls"
case "$*" in
  'runtime-source'|'inspect-runtime --json')
    printf "error: unrecognized subcommand '%s'\n" "$1" >&2
    exit 2 ;;
  'status --json') exec cat "$CTL_TEST_DIR/legacy-status.json" ;;
  'catalog-json') exec cat "$CTL_TEST_DIR/legacy-catalog.json" ;;
  *) printf 'unexpected legacy manager command\n' >&2; exit 90 ;;
esac
"#;

fn legacy_fixture(channels: &[(Option<&str>, bool)]) -> (Fixture, Manifest) {
  let fixture = Fixture::new(EXPECTED_ID);
  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.1.4".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let components = serde_json::from_value(Fixture::compatible_components(&build)).unwrap();
  let files = ctl_core::bundle_layout::ALL
    .into_iter()
    .map(|name| (name.to_owned(), b"#!/bin/sh\nexit 0\n".to_vec()))
    .collect();
  let manifest = Manifest::new(fixture_target(), Source::Ci, components, &files).unwrap();
  let rows: Vec<_> = channels
    .iter()
    .map(|&(channel, current)| {
      json!({
        "bundle_id": manifest.bundle_id, "target_triple": manifest.target_triple, "current": current,
        "origin": channel.map(|channel| json!({"channel": channel, "publication_id": "v0.1.4"}))
      })
    })
    .collect();
  fs::write(
    fixture.0.join("legacy-status.json"),
    serde_json::to_vec(&json!({"active": manifest, "previous": null})).unwrap(),
  )
  .unwrap();
  fs::write(
    fixture.0.join("legacy-catalog.json"),
    serde_json::to_vec(&json!({"schema_version": 1, "installations": rows})).unwrap(),
  )
  .unwrap();
  fs::create_dir_all(fixture.0.join("home/.tokn/ctl/bin")).unwrap();
  fixture.script("home/.tokn/ctl/bin/ctlup", LEGACY_MANAGER);
  (fixture, manifest)
}

async fn run(fixture: &Fixture, action: &str) -> Output {
  let broker = fixture.updater_broker();
  let output = fixture
    .command(
      &[action, "--host", "work", "--json"],
      Path::new(env!("CARGO_BIN_EXE_ctlup")),
      None,
    )
    .await;
  broker.await.unwrap();
  output
}

fn assert_passive(fixture: &Fixture, expected_calls: &[&str]) {
  let calls = fs::read_to_string(fixture.0.join("manager-calls")).unwrap();
  assert_eq!(calls.lines().collect::<Vec<_>>(), expected_calls);
  assert!(fixture.0.join("account-id").is_file());
  let count: usize = fs::read_to_string(fixture.0.join("count"))
    .unwrap()
    .trim()
    .parse()
    .unwrap();
  fixture.assert_managed_channels(count);
  assert!(fixture.arguments(1).join("\n").contains("remote-id"));
  let manager = fs::read_to_string(fixture.0.join("home/.tokn/ctl/bin/ctlup")).unwrap();
  assert!(manager.contains(LEGACY_MANAGER));
  for marker in [
    "bootstrap",
    "preflight",
    "upload",
    "builder-called",
    "gh-called",
    "home/.tokn/ctl/ctlup/state/installations.json",
  ] {
    assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_exposes_the_verified_active_build_and_legacy_catalog_in_schema_two() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let (fixture, manifest) = legacy_fixture(&[(Some("nightly"), true)]);
  let output = run(&fixture, "status").await;
  assert!(output.status.success(), "{output:?}");
  let status: ctl_client::installation::InstallationShow =
    serde_json::from_slice(&output.stdout).unwrap();
  status.validate().unwrap();
  assert_eq!(status.schema_version, 2);
  assert_eq!(status.active.as_ref(), Some(&manifest.bundle_id));
  assert_eq!(status.active_origin.unwrap().channel, "nightly");
  assert_eq!(status.builds[0].target_triple, manifest.target_triple);
  assert_eq!(status.builds[0].build.version, "0.1.4");
  assert_passive(&fixture, &["status --json", "catalog-json"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_preserves_a_uniquely_pinned_legacy_bundle() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let (fixture, manifest) = legacy_fixture(&[(Some("provided"), true)]);
  let output = run(&fixture, "update").await;
  assert!(output.status.success(), "{output:?}");
  let [result]: [Value; 1] = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(result["check"]["state"], "pinned");
  assert_eq!(result["check"]["active"], manifest.bundle_id);
  assert!(result["result"].is_null());
  assert_passive(
    &fixture,
    &["runtime-source", "status --json", "catalog-json"],
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_requires_a_source_when_legacy_active_bundle_has_two_memberships() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let (fixture, _) = legacy_fixture(&[(Some("stable"), false), (Some("nightly"), true)]);
  let output = run(&fixture, "update").await;
  assert!(!output.status.success(), "{output:?}");
  let [result]: [Value; 1] = serde_json::from_slice(&output.stdout).unwrap();
  let error = result["error"].as_str().unwrap();
  assert!(error.contains("choose a source"), "{error}");
  assert!(result["result"].is_null());
  assert_passive(
    &fixture,
    &["runtime-source", "status --json", "catalog-json"],
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_preserves_the_build_with_an_unidentified_legacy_membership() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let (fixture, manifest) = legacy_fixture(&[(None, false), (Some("stable"), true)]);
  let output = run(&fixture, "status").await;
  assert!(output.status.success(), "{output:?}");
  let status: ctl_client::installation::InstallationShow =
    serde_json::from_slice(&output.stdout).unwrap();
  status.validate().unwrap();
  assert_eq!(status.active.as_ref(), Some(&manifest.bundle_id));
  assert_eq!(status.builds[0].bundle_id, manifest.bundle_id);
  assert!(status.active_origin.is_none());
  assert_passive(&fixture, &["status --json", "catalog-json"]);
}
