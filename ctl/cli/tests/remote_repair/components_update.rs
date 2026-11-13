//! Explicit updates bootstrap an old agent through its persistent account ID.
use super::*;

const OTHER_ID: &str = "0c9247af-580a-4e90-9a88-45a4f76c0ca7";

fn catalogued_agent_without_inspect(fixture: &Fixture, remote_id: Option<&str>) -> PathBuf {
  use ctl_core::bundles::{Manifest, Source, Store};
  use ctl_core::installations::{CatalogOrigin, Installations};

  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.0.8".into(),
    source_revision: Some("b".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let publication = "0.0.8-dev.bbbbbbbbbbbb";
  let components = serde_json::from_value(Fixture::compatible_components(&build)).unwrap();
  let files = ctl_core::bundle_layout::ALL
    .into_iter()
    .map(|name| {
      let bytes = if name == "ctl-agent" {
        b"#!/bin/sh\nprintf \"error: unrecognized subcommand 'inspect'\\n\" >&2\nexit 2\n"
          .as_slice()
      } else {
        b"#!/bin/sh\n# retained companion\nexit 0\n".as_slice()
      };
      (name.into(), bytes.to_vec())
    })
    .collect();
  let manifest = Manifest::new(fixture_target(), Source::Ci, components, &files)
    .unwrap()
    .with_distribution_id(publication)
    .unwrap();
  let home = fixture.0.join("home");
  let source = Store::cache(&home).publish(&manifest, &files).unwrap();
  let installed = Installations::new(&home)
    .activate_with_origin(
      &source,
      &CatalogOrigin {
        channel: "provided".into(),
        publication_id: Some(publication.into()),
        built_at: None,
      },
    )
    .unwrap();
  set_remote_id(fixture, remote_id);
  installed.directory
}

fn set_remote_id(fixture: &Fixture, remote_id: Option<&str>) {
  let path = fixture.0.join("home/.tokn/ctl/remote-id");
  if let Some(remote_id) = remote_id {
    fs::write(path, remote_id).unwrap();
  } else if path.exists() {
    fs::remove_file(path).unwrap();
  }
}

fn legacy_installation(fixture: &Fixture, remote_id: Option<&str>) -> PathBuf {
  let base = fixture.0.join("home/.tokn/ctl");
  let legacy = base.join("versions/legacy");
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
  std::os::unix::fs::symlink("versions/legacy", base.join("current")).unwrap();
  set_remote_id(fixture, remote_id);
  legacy
}

fn source(fixture: &Fixture) -> String {
  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.0.9".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "a".repeat(64),
    dirty: false,
  };
  fixture.compatible_bundle(&build)
}

fn update(fixture: &Fixture) -> Terminal {
  Terminal::with_args(
    fixture,
    &[
      "__ctlup",
      "-H",
      "work",
      "update",
      "--from",
      fixture.0.join("bundles").to_str().unwrap(),
    ],
  )
}

fn assert_update_channels(fixture: &Fixture) {
  fixture.assert_managed_channels(7);
  for (index, command) in [
    (1, "remote-id"),
    (2, "ctl-platform-v1"),
    (3, "ctlup-bootstrap-v1"),
    (4, "ctlup-bootstrap-v1"),
    (5, "activate-installed"),
    (6, "install-archive"),
    (7, "exec ctl-agent inspect"),
  ] {
    assert!(fixture.arguments(index).join("\n").contains(command));
  }
  assert!(!fixture.0.join("builder-called").exists());
  assert!(!fixture.0.join("service-opens").exists());
}

fn assert_installed_bundle(fixture: &Fixture, bundle_id: &str) {
  let account = fixture.0.join("home/.tokn/ctl");
  let current = account.join("current");
  let installed = ctl_core::bundles::Bundle::open(&fs::canonicalize(&current).unwrap()).unwrap();
  assert_eq!(
    installed.manifest.distribution_id.as_deref(),
    Some(bundle_id)
  );
  assert_eq!(
    fs::read_link(&current).unwrap(),
    PathBuf::from("installs").join(&installed.manifest.bundle_id)
  );
  assert_eq!(
    fs::read_to_string(account.join("remote-id")).unwrap(),
    EXPECTED_ID
  );
  let payload = fixture.0.join("payload");
  for file in [
    "ctl-agent",
    "ctmuxd",
    "ctl-taskd",
    "ctld",
    "ctl",
    "ctmux",
    "manifest.json",
  ] {
    assert_eq!(
      fs::read(current.join(file)).unwrap(),
      fs::read(payload.join(file)).unwrap()
    );
  }
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_downloaded_manager_filename_can_deploy_offline_to_its_native_target() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new(EXPECTED_ID);
  source(&fixture);
  catalogued_agent_without_inspect(&fixture, Some(EXPECTED_ID));
  fixture.prepare_updater();
  let executable = fixture.0.join("ctlup-fixture-manager");
  let broker = fixture.updater_broker();
  let mut terminal = Terminal::with_executable(
    &fixture,
    &[
      "--host",
      "work",
      "update",
      "--from",
      fixture.0.join("bundles").to_str().unwrap(),
    ],
    &executable,
    None,
  );
  assert!(terminal.finish().success(), "{}", terminal.text());
  assert_update_channels(&fixture);
  assert_eq!(
    Sha256::digest(fs::read(fixture.0.join("home/.tokn/ctl/bin/ctlup")).unwrap()),
    Sha256::digest(fs::read(executable).unwrap()),
  );
  broker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_agent_without_inspect_can_update_the_complete_build_and_verify_activation() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  {
    let fixture = Fixture::new(EXPECTED_ID);
    let bundle_id = source(&fixture);
    let legacy = catalogued_agent_without_inspect(&fixture, Some(EXPECTED_ID));
    let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
    let old_inspect = Command::new(legacy.join("ctl-agent"))
      .arg("inspect")
      .output()
      .unwrap();
    assert_eq!(old_inspect.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&old_inspect.stderr).contains("unrecognized subcommand"));
    let broker = fixture.updater_broker();
    let mut terminal = update(&fixture);
    assert!(terminal.finish().success(), "{}", terminal.text());
    assert!(
      terminal
        .text()
        .contains("installed; running services preserved")
    );
    assert_update_channels(&fixture);
    assert_installed_bundle(&fixture, &bundle_id);
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
    let legacy = catalogued_agent_without_inspect(&fixture, remote_id);
    let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
    let broker = fixture.updater_broker();
    let mut terminal = update(&fixture);
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
    catalogued_agent_without_inspect(&fixture, Some(EXPECTED_ID));
    let path = fixture.0.join("upgraded-identity");
    let mut identity: serde_json::Value =
      serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    if changed_account {
      identity["remote_id"] = json!(OTHER_ID);
    } else {
      identity["bundle"]["bundle_id"] = json!("different-installed-bundle");
    }
    fs::write(path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let broker = fixture.updater_broker();
    let mut terminal = update(&fixture);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmanaged_layout_blocks_update_before_ssh_and_preserves_the_old_installation() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new(EXPECTED_ID);
  source(&fixture);
  let legacy = legacy_installation(&fixture, Some(EXPECTED_ID));
  let account = fixture.0.join("home/.tokn/ctl");
  let current = fs::read_link(account.join("current")).unwrap();
  let old_payloads = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"]
    .map(|name| (name, fs::read(legacy.join(name)).unwrap()));
  let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
  // This fixture shares the local preparation account with its simulated SSH
  // account. Its old local layout must be refused before opening any channel.
  let mut terminal = update(&fixture);
  assert!(!terminal.finish().success(), "{}", terminal.text());
  assert!(
    terminal.text().contains("run ctlup migrate"),
    "{}",
    terminal.text()
  );
  for marker in ["count", "bootstrap", "preflight", "upload", "inspect"] {
    assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
  }
  assert_eq!(fs::read_link(account.join("current")).unwrap(), current);
  for (name, bytes) in old_payloads {
    assert_eq!(fs::read(legacy.join(name)).unwrap(), bytes);
  }
  assert!(!account.join("catalog.json").exists());
  assert!(!account.join("installs").exists());
  assert_eq!(
    fs::read(fixture.0.join("hosts.json")).unwrap(),
    original_hosts
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_provided_bundle_is_rejected_before_bootstrap_or_upload() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new(EXPECTED_ID);
  let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.updater_broker();
  let mut terminal = update(&fixture);
  assert!(!terminal.finish().success(), "{}", terminal.text());
  assert!(
    terminal.text().contains("invalid remote bundle-set JSON"),
    "{}",
    terminal.text()
  );
  fixture.assert_managed_channels(2);
  assert!(fixture.arguments(1).join("\n").contains("remote-id"));
  assert!(fixture.arguments(2).join("\n").contains("ctl-platform-v1"));
  for marker in [
    "bootstrap",
    "preflight",
    "upload",
    "inspect",
    "builder-called",
  ] {
    assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
  }
  assert!(!fixture.0.join("home/.tokn/ctl/current").exists());
  assert_eq!(
    fs::read(fixture.0.join("hosts.json")).unwrap(),
    original_hosts
  );
  broker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_update_preserves_older_cached_and_selected_sources() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new(EXPECTED_ID);
  let previous = fixture.stale_sources();
  let bundle_id = source(&fixture);
  let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.updater_broker();
  let mut terminal = update(&fixture);
  assert!(terminal.finish().success(), "{}", terminal.text());
  assert_update_channels(&fixture);
  assert_installed_bundle(&fixture, &bundle_id);
  assert_selection(&fixture, Some(&previous.manifest.bundle_id));
  let cache = ctl_client::remote_bundle::BundleCacheEntry::new(
    &fixture.0.join("home/.tokn/ctl/agent-bundles"),
    fixture_target(),
    &previous.manifest.components["ctl-agent"].build,
  )
  .unwrap();
  let retained = cache.load().unwrap().unwrap();
  assert_eq!(Some(retained.bundle_id), previous.manifest.distribution_id);
  assert_eq!(
    fs::read(fixture.0.join("hosts.json")).unwrap(),
    original_hosts
  );
  broker.await.unwrap();
}
