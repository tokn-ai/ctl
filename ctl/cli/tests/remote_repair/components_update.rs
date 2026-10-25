//! Explicit updates bootstrap an old agent through its persistent account ID.
use super::*;

const OTHER_ID: &str = "0c9247af-580a-4e90-9a88-45a4f76c0ca7";

fn legacy_installation(fixture: &Fixture, remote_id: Option<&str>) -> PathBuf {
  let base = fixture.0.join("home/.tokn/ctl");
  let legacy = base.join("legacy");
  fs::create_dir_all(&legacy).unwrap();
  fs::write(
    legacy.join("ctl-agent"),
    b"#!/bin/sh\nprintf \"error: unrecognized subcommand 'inspect'\\n\" >&2\nexit 2\n",
  )
  .unwrap();
  for name in ["ctmuxd", "ctl-taskd", "ctld"] {
    fs::write(
      legacy.join(name),
      b"#!/bin/sh\n# retained companion\nexit 0\n",
    )
    .unwrap();
  }
  for name in ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"] {
    fs::set_permissions(legacy.join(name), fs::Permissions::from_mode(0o700)).unwrap();
  }
  std::os::unix::fs::symlink("legacy", base.join("current")).unwrap();
  if let Some(remote_id) = remote_id {
    fs::write(base.join("remote-id"), remote_id).unwrap();
  } else if base.join("remote-id").exists() {
    fs::remove_file(base.join("remote-id")).unwrap();
  }
  legacy
}

fn source(fixture: &Fixture) -> String {
  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.0.9".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "a".repeat(64),
    dirty: false,
  };
  let bundle_id = fixture.compatible_bundle(&build);
  let manifest: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.0.join("payload/manifest.json")).unwrap()).unwrap();
  let path = fixture.0.join("upgraded-identity");
  let mut identity: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
  identity["protocols"] = manifest["components"]["ctl-agent"]["protocols"].clone();
  fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
  bundle_id
}

fn update(fixture: &Fixture, package: &str) -> Terminal {
  Terminal::with_args(
    fixture,
    &[
      "-H",
      "work",
      "components",
      "update",
      "--package",
      package,
      "--from",
      fixture.0.join("bundles").to_str().unwrap(),
    ],
  )
}

fn assert_update_channels(fixture: &Fixture) {
  fixture.assert_managed_channels(4);
  for (index, command) in [
    (1, "remote-id"),
    (2, "ctl-platform-v1"),
    (3, "ctl-install-progress-v1"),
    (4, "exec ctl-agent inspect"),
  ] {
    assert!(fixture.arguments(index).join("\n").contains(command));
  }
  assert!(!fixture.0.join("builder-called").exists());
  assert!(!fixture.0.join("service-input-1").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_agent_without_inspect_can_update_both_packages_and_verify_activation() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for package in ["full-bundle", "ctl-agent"] {
    let fixture = Fixture::new(EXPECTED_ID);
    let bundle_id = source(&fixture);
    let legacy = legacy_installation(&fixture, Some(EXPECTED_ID));
    let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
    let old_inspect = Command::new(legacy.join("ctl-agent"))
      .arg("inspect")
      .output()
      .unwrap();
    assert_eq!(old_inspect.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&old_inspect.stderr).contains("unrecognized subcommand"));
    let broker = fixture.broker_with_observation(false);
    let mut terminal = update(&fixture, package);
    assert!(terminal.finish().success(), "{}", terminal.text());
    assert!(
      terminal
        .text()
        .contains("installed; running services preserved")
    );
    assert_update_channels(&fixture);
    let current = fixture.0.join("home/.tokn/ctl/current");
    if package == "full-bundle" {
      let installed =
        ctl_core::bundles::Bundle::open(&fs::canonicalize(&current).unwrap()).unwrap();
      assert_eq!(
        installed.manifest.distribution_id.as_deref(),
        Some(bundle_id.as_str())
      );
    } else {
      for companion in ["ctmuxd", "ctl-taskd", "ctld"] {
        assert_eq!(
          fs::canonicalize(current.join(companion)).unwrap(),
          fs::canonicalize(legacy.join(companion)).unwrap(),
        );
      }
    }
    assert_eq!(
      fs::read_to_string(current.parent().unwrap().join("remote-id")).unwrap(),
      EXPECTED_ID
    );
    assert_eq!(
      fs::read(fixture.0.join("hosts.json")).unwrap(),
      original_hosts
    );
    assert_selection(&fixture, None);
    broker.await.unwrap();
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_or_changed_account_id_blocks_before_platform_source_or_upload() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for remote_id in [None, Some(OTHER_ID)] {
    let fixture = Fixture::new(EXPECTED_ID);
    let legacy = legacy_installation(&fixture, remote_id);
    let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
    let broker = fixture.broker_with_observation(false);
    let mut terminal = update(&fixture, "full-bundle");
    assert!(!terminal.finish().success(), "{}", terminal.text());
    if remote_id.is_some() {
      assert!(terminal.text().contains("different remote environment"));
    }
    fixture.assert_managed_channels(1);
    assert!(fixture.arguments(1).join("\n").contains("remote-id"));
    for marker in ["platform", "upload", "inspect", "builder-called"] {
      assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
    }
    assert!(!fixture.0.join("home/.tokn/ctl/components").exists());
    assert_eq!(
      fs::canonicalize(fixture.0.join("home/.tokn/ctl/current")).unwrap(),
      fs::canonicalize(legacy).unwrap(),
    );
    assert_eq!(
      fs::read(fixture.0.join("hosts.json")).unwrap(),
      original_hosts
    );
    broker.await.unwrap();
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newly_installed_agent_must_still_report_the_saved_account_and_exact_activation() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for changed_account in [true, false] {
    let fixture = Fixture::new(EXPECTED_ID);
    source(&fixture);
    legacy_installation(&fixture, Some(EXPECTED_ID));
    let path = fixture.0.join("upgraded-identity");
    let mut identity: serde_json::Value =
      serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    if changed_account {
      identity["remote_id"] = json!(OTHER_ID);
    } else {
      identity["bundle"]["bundle_id"] = json!("different-installed-bundle");
    }
    fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let broker = fixture.broker_with_observation(false);
    let mut terminal = update(&fixture, "full-bundle");
    assert!(!terminal.finish().success(), "{}", terminal.text());
    let expected = if changed_account {
      "different remote environment"
    } else {
      "activation could not be verified"
    };
    assert!(terminal.text().contains(expected), "{}", terminal.text());
    assert_update_channels(&fixture);
    assert!(fixture.0.join("upload").exists());
    assert!(
      !terminal
        .text()
        .contains("installed; running services preserved")
    );
    broker.await.unwrap();
  }
}
