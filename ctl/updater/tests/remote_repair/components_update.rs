//! Explicit updates bootstrap an old agent through its persistent account ID.
use super::*;
use std::path::Path;

fn run_published_receiver(
  fixture: &Fixture,
  archive_name: &str,
  supplied: Option<&[u8]>,
) -> std::process::Output {
  use std::io::Write as _;
  use std::process::Stdio;

  let target = fixture_target();
  let url = format!(
    "{}sets/demo/{archive_name}",
    ctl_client::remote_bundle::published::ROOT
  );
  let manifest = fs::read(fixture.0.join("bundles/bundle-set.json")).unwrap();
  let origin =
    ctl_client::installation::encode_catalog_origin(&ctl_core::installations::CatalogOrigin {
      channel: "nightly".into(),
      built_at: None,
      publication_id: Some("demo".into()),
    })
    .unwrap();
  let mut command = Command::new(fixture.0.join("ctlup-fixture-manager"));
  command
    .args([
      "install-publication",
      "--origin-hex",
      &origin,
      "--target",
      target,
      "--archive-url",
      &url,
      "--manifest-bytes",
      &manifest.len().to_string(),
    ])
    .env("HOME", fixture.0.join("home"))
    .env_remove("CTL_DATA_DIR")
    .env("PATH", format!("{}:/usr/bin:/bin", fixture.0.display()))
    .env("TEST_ARCHIVE", fixture.0.join("bundles").join(archive_name))
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  if supplied.is_some() {
    command.arg("--archive-stdin");
  }
  let mut child = command.spawn().unwrap();
  let mut stdin = child.stdin.take().unwrap();
  stdin.write_all(&manifest).unwrap();
  if let Some(bytes) = supplied {
    stdin.write_all(bytes).unwrap();
  }
  drop(stdin);
  child.wait_with_output().unwrap()
}

#[test]
fn published_receiver_checks_the_download_then_reuses_the_installed_build() {
  let fixture = Fixture::new(EXPECTED_ID);
  fixture.prepare_updater();
  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.1.0".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "b".repeat(64),
    dirty: false,
  };
  let publication = fixture.compatible_bundle(&build);
  let target = fixture_target();
  let name = format!("ctl-agent-bundle-{publication}-{target}.tar.gz");
  let run = |supplied: Option<&[u8]>| run_published_receiver(&fixture, &name, supplied);
  fixture.script("curl", "exit 7");
  fixture.script("wget", "exit 4");
  let unavailable = run(None);
  assert_eq!(unavailable.status.code(), Some(12), "{unavailable:?}");
  assert_eq!(unavailable.stdout, b"ctl-published-unavailable-v1\n");
  let damaged = run(Some(b"corrupt cached archive"));
  assert_eq!(damaged.status.code(), Some(1), "{damaged:?}");
  assert_eq!(damaged.stdout, b"");
  let home = fixture.0.join("home");
  let installations = ctl_core::installations::Installations::new(&home);
  assert!(installations.active().unwrap().is_none());
  assert!(!home.join(".tokn/ctl/bin/ctlup").exists());

  fixture.script("wget", "exec cat \"$TEST_ARCHIVE\"");
  let installed = run(None);
  assert!(installed.status.success(), "{installed:?}");
  let encoded = installed
    .stdout
    .strip_prefix(b"ctl-published-install-v1\n")
    .unwrap();
  let installed: ctl_core::bundles::Manifest = serde_json::from_slice(encoded).unwrap();
  assert_eq!(installations.active().unwrap().unwrap().manifest, installed);
  assert_eq!(
    installed.distribution_id.as_deref(),
    Some(publication.as_str())
  );
  assert!(home.join(".tokn/ctl/bin/ctlup").is_file());
  assert!(
    installations
      .catalog()
      .unwrap()
      .iter()
      .any(|entry| entry.current
        && entry
          .origin
          .as_ref()
          .is_some_and(|origin| origin.channel == "nightly"
            && origin.publication_id.as_deref() == Some("demo")))
  );
  fixture.script("wget", "exit 8");
  let manager = home.join(".tokn/ctl/bin/ctlup");
  let before = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&manager).unwrap());
  let reused = run(None);
  assert!(reused.status.success(), "{reused:?}");
  assert_eq!(
    std::os::unix::fs::MetadataExt::ino(&fs::metadata(manager).unwrap()),
    before
  );
  assert!(
    reused
      .stdout
      .starts_with(b"ctl-published-reused-v1\nctl-published-install-v1\n")
  );
}

const OTHER_ID: &str = "0c9247af-580a-4e90-9a88-45a4f76c0ca7";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_dev_builds_from_the_explicit_workspace_after_verifying_the_account() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new(EXPECTED_ID);
  let workspace = fixture.0.join("dev checkout");
  fs::create_dir_all(workspace.join("scripts/dev")).unwrap();
  fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
  fs::write(workspace.join("scripts/dev/build-agent-bundle.mts"), "").unwrap();
  fixture.script(
    "uname",
    r#"case "$1" in -s) printf 'Linux\n' ;; -m) printf 'aarch64\n' ;; *) exit 1 ;; esac"#,
  );
  fixture.script(
    "node",
    r#"
test -f "$CTL_TEST_DIR/account-id"
test -f "$CTL_TEST_DIR/platform"
checkout=$(cd "$CTL_TEST_DIR/dev checkout" && pwd -P)
test "$(pwd -P)" = "$checkout"
test "$3" = "$checkout/scripts/dev/build-agent-bundle.mts"
test "$4" = --target
test "$5" = aarch64-unknown-linux-musl
touch "$CTL_TEST_DIR/builder-called"
printf 'fixture cross-build failed\n' >&2
exit 1
"#,
  );
  let broker = fixture.updater_broker();
  let output = fixture
    .command(
      &[
        "--host",
        "work",
        "dev",
        "--workspace",
        workspace.to_str().unwrap(),
        "--json",
      ],
      Path::new(env!("CARGO_BIN_EXE_ctlup")),
      None,
    )
    .await;
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success(), "{diagnostics}");
  assert!(
    diagnostics.contains("fixture cross-build failed"),
    "{diagnostics}"
  );
  let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(result[0]["host"], "work");
  assert!(result[0]["result"].is_null());
  assert!(fixture.0.join("builder-called").is_file());
  assert!(!fixture.0.join("upload").exists());
  assert!(!fixture.0.join("gh-called").exists());
  fixture.assert_managed_channels(2);
  broker.await.unwrap();
}

#[tokio::test]
async fn remote_development_rejects_unrelated_cwd_before_ssh() {
  let fixture = Fixture::new(EXPECTED_ID);
  let output = fixture
    .command(
      &["--host", "work", "dev", "--json"],
      Path::new(env!("CARGO_BIN_EXE_ctlup")),
      None,
    )
    .await;
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success(), "{diagnostics}");
  assert!(diagnostics.contains("ctl checkout root"), "{diagnostics}");
  assert_eq!(output.stdout, [] as [u8; 0]);
  for marker in ["ssh-1.args", "account-id", "platform", "builder-called"] {
    assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
  }
}

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

async fn update(fixture: &Fixture) -> std::process::Output {
  fixture.prepare_updater();
  fixture
    .command(
      &[
        "-H",
        "work",
        "update",
        "--from",
        fixture.0.join("bundles").to_str().unwrap(),
      ],
      std::path::Path::new(env!("CARGO_BIN_EXE_ctlup")),
      Some(&fixture.0.join("ctlup-fixture-manager")),
    )
    .await
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
  let output = fixture
    .command(
      &[
        "--host",
        "work",
        "update",
        "--from",
        fixture.0.join("bundles").to_str().unwrap(),
      ],
      &executable,
      None,
    )
    .await;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
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
    let output = update(&fixture).await;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostics}");
    assert!(diagnostics.contains("installed; running services preserved"));
    assert_update_channels(&fixture);
    assert_installed_bundle(&fixture, &bundle_id);
    assert_eq!(
      fs::read(fixture.0.join("hosts.json")).unwrap(),
      original_hosts
    );
    assert_selection(&fixture, None);
    broker.await.unwrap();

    // Repeat the update through the same saved route. The remote account
    // already has both the target updater and the verified installation.
    fs::remove_file(fixture.0.join("ctld.sock")).unwrap();
    fs::remove_file(fixture.0.join("allow-upload")).unwrap();
    let broker = fixture.updater_broker();
    let output = update(&fixture).await;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{diagnostics}");
    assert!(
      diagnostics.contains("Reused installed remote components"),
      "{diagnostics}"
    );
    assert!(!diagnostics.contains("ctlup: Sending"), "{diagnostics}");
    assert!(!diagnostics.contains("ctlup: Extracting"), "{diagnostics}");
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
    let output = update(&fixture).await;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{diagnostics}");
    if remote_id.is_some() {
      assert!(diagnostics.contains("different remote environment"));
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
    let output = update(&fixture).await;
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{diagnostics}");
    let expected = if changed_account {
      "different remote environment"
    } else {
      "activation could not be verified"
    };
    assert!(diagnostics.contains(expected), "{diagnostics}");
    assert_update_channels(&fixture);
    assert!(fixture.0.join("upload").exists());
    assert!(!diagnostics.contains("installed; running services preserved"));
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
  let output = update(&fixture).await;
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success(), "{diagnostics}");
  assert!(diagnostics.contains("run ctlup migrate"), "{diagnostics}");
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
  let output = update(&fixture).await;
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success(), "{diagnostics}");
  assert!(
    diagnostics.contains("invalid remote bundle-set JSON"),
    "{diagnostics}"
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
  let output = update(&fixture).await;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
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
