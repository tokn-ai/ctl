#![cfg(unix)]

use ctl_core::bundles::{MANIFEST_FILE, Manifest, Source};
use ctl_core::component::{ComponentBuildInfo, ComponentInfo, ProtocolInfo};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Home(PathBuf);
impl Home {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctlup-cli-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Self(path)
  }

  fn command(&self, args: &[&str]) -> Command {
    self.command_at(Path::new(env!("CARGO_BIN_EXE_ctlup")), args)
  }

  fn command_at(&self, executable: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(executable);
    command
      .args(args)
      .env("HOME", &self.0)
      .env("USERPROFILE", &self.0);
    command.env_remove("CTL_REMOTE_BUNDLES_DIR");
    command.env_remove("CTL_BUNDLE_CHANNEL");
    command
  }

  fn install(&self, archive: &[u8]) -> Output {
    let mut child = self
      .command(&["install-archive"])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap();
    child.stdin.take().unwrap().write_all(archive).unwrap();
    child.wait_with_output().unwrap()
  }
}
impl Drop for Home {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn fixture_payload(revision: char) -> (BTreeMap<String, ComponentInfo>, BTreeMap<String, Vec<u8>>) {
  let build = ComponentBuildInfo {
    version: "0.1.2".into(),
    source_revision: Some(revision.to_string().repeat(40)),
    source_fingerprint: revision.to_string().repeat(64),
    dirty: false,
  };
  let mut protocols = ctl_proto::agent_protocols();
  for protocol in ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols {
    if !protocols.contains(&protocol) {
      protocols.push(protocol);
    }
  }
  protocols.extend([
    ctmux_proto::protocol_info(),
    ctmux_ipc::local_control_protocol_info(),
    ctl_task_proto::protocol_info(),
    ctl_task_proto::control::protocol_info(),
    ProtocolInfo::new(
      "ctl_remote_vpn",
      ctl_ipc::remote_vpn::PROTOCOL_BUILD,
      ctl_ipc::remote_vpn::PROTOCOL_VERSION,
      ctl_ipc::remote_vpn::SUPPORTED_PROTOCOL_VERSIONS,
    ),
  ]);
  let components = ctl_core::bundle_layout::ALL
    .into_iter()
    .map(|name| {
      (
        name.to_owned(),
        ComponentInfo {
          build: build.clone(),
          protocols: protocols.clone(),
        },
      )
    })
    .collect();
  let files = ctl_core::bundle_layout::ALL
    .into_iter()
    .map(|name| {
      (
        name.to_owned(),
        format!("#!/bin/sh\nprintf '%s\\n' '{revision}-{name}'\n").into_bytes(),
      )
    })
    .collect();
  (components, files)
}

fn archive(revision: char) -> (Manifest, Vec<u8>) {
  let (components, files) = fixture_payload(revision);
  let manifest = Manifest::new(
    ctl_core::paths::native_target(),
    Source::Ci,
    components,
    &files,
  )
  .unwrap();
  let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
  let mut builder = tar::Builder::new(encoder);
  let mut contents = files;
  contents.insert(MANIFEST_FILE.into(), serde_json::to_vec(&manifest).unwrap());
  for (name, bytes) in contents {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(if name == MANIFEST_FILE { 0o600 } else { 0o700 });
    header.set_cksum();
    builder
      .append_data(&mut header, name, bytes.as_slice())
      .unwrap();
  }
  (manifest, builder.into_inner().unwrap().finish().unwrap())
}

#[test]
fn explicit_catalog_migration_preserves_selection_and_records_component_memberships() {
  let home = Home::new();
  let (previous, bytes) = archive('a');
  assert!(home.install(&bytes).status.success());
  let (active, bytes) = archive('b');
  assert!(home.install(&bytes).status.success());
  let root = home.0.join(".tokn/ctl");
  let selection = fs::read_link(root.join("current")).unwrap();
  let rollback = fs::read_link(root.join("previous")).unwrap();
  // Recreate an older complete installation whose payloads predate the catalog.
  fs::remove_file(root.join("catalog.json")).unwrap();
  let inspection = home.command(&["show", "--json"]).output().unwrap();
  assert!(!inspection.status.success());
  assert!(String::from_utf8_lossy(&inspection.stderr).contains("ctlup migrate"));
  assert!(!root.join("catalog.json").exists());

  let output = home.command(&["migrate", "--json"]).output().unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let catalog: ctl_client::installation::InstallationCatalog =
    serde_json::from_slice(&output.stdout).unwrap();
  catalog.validate().unwrap();
  assert_eq!(catalog.installations.len(), 2);
  assert!(catalog.installations.iter().all(|entry| {
    entry
      .origin
      .as_ref()
      .is_some_and(|origin| origin.channel == "provided" && origin.built_at.is_none())
  }));
  assert_eq!(fs::read_link(root.join("current")).unwrap(), selection);
  assert_eq!(fs::read_link(root.join("previous")).unwrap(), rollback);
  for manifest in [&active, &previous] {
    assert!(root.join("installs").join(&manifest.bundle_id).is_dir());
  }
  let recorded = fs::read(root.join("catalog.json")).unwrap();
  let repeated = home.command(&["migrate", "--json"]).output().unwrap();
  assert!(repeated.status.success());
  assert_eq!(recorded, fs::read(root.join("catalog.json")).unwrap());

  let output = home
    .command(&["component", "list", "--installed"])
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(
    String::from_utf8(output.stdout).unwrap().trim(),
    format!("bundle-{} (provided)", ctl_core::paths::native_target())
  );
}

#[test]
fn migration_rejects_remote_options_before_preparing_storage() {
  let home = Home::new();
  let output = home
    .command(&["--host", "unused", "migrate"])
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("operation is local"));
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn passive_status_and_metadata_do_not_prepare_an_installation() {
  let home = Home::new();
  let output = home.command(&["status", "--json"]).output().unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(value["active"].is_null());
  assert!(value["previous"].is_null());
  let output = home.command(&["--component-info"]).output().unwrap();
  assert!(output.status.success());
  let info: ComponentInfo = serde_json::from_slice(&output.stdout).unwrap();
  assert!(info.is_valid());
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn empty_status_explains_managed_installations_and_scopes_the_development_hint() {
  let home = Home::new();
  let output = home
    .command(&["status"])
    .current_dir(&home.0)
    .output()
    .unwrap();
  assert!(output.status.success());
  let text = String::from_utf8(output.stdout).unwrap();
  assert!(text.contains("No managed ctl installation."));
  assert!(text.contains("ctlup install --help"));
  assert!(!text.contains("ctlup dev"));

  let checkout = home.0.join("checkout");
  fs::create_dir_all(checkout.join("ctl/cli")).unwrap();
  fs::create_dir_all(checkout.join("scripts/dev")).unwrap();
  for path in [
    ".git",
    "Cargo.toml",
    "ctl/cli/Cargo.toml",
    "scripts/dev/ctld-signed.mts",
  ] {
    fs::write(checkout.join(path), b"fixture").unwrap();
  }
  let output = home
    .command(&["status"])
    .current_dir(checkout.join("ctl/cli"))
    .output()
    .unwrap();
  assert!(output.status.success());
  let text = String::from_utf8(output.stdout).unwrap();
  assert!(text.contains("No managed ctl installation."));
  assert!(text.contains("ctlup dev"));
  assert!(
    !home.0.join(".tokn").exists(),
    "status never activates or adopts development helpers"
  );
}

#[test]
fn updater_executable_preserves_the_shared_private_askpass_entrypoint() {
  let home = Home::new();
  let output = home
    .command(&["private prompt"])
    .env("CTL_CALLER_ASKPASS", "1")
    .env("SSH_ASKPASS_PROMPT", "confirm")
    .output()
    .unwrap();
  assert!(!output.status.success());
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(
    diagnostics.contains("not supported with a supplied credential"),
    "{diagnostics}"
  );
  assert!(!diagnostics.contains("unrecognized subcommand"));
  assert_eq!(output.stdout, [] as [u8; 0]);
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn full_storage_override_provisions_an_independent_installation() {
  let home = Home::new();
  let root = home.0.join("alternate");
  fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
  let (manifest, bytes) = archive('a');
  let mut child = home
    .command(&["install-archive"])
    .env("CTL_DATA_DIR", &root)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(&bytes).unwrap();
  let output = child.wait_with_output().unwrap();
  assert!(output.status.success(), "{output:?}");
  assert!(root.join("installs").join(manifest.bundle_id).is_dir());
  assert!(root.join("bin/ctl").is_file());
  let output = home
    .command(&["status", "--json"])
    .env("CTL_DATA_DIR", &root)
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(!status["active"].is_null());
  assert!(!home.0.join(".tokn").exists());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn install_from_files_provisions_the_independent_manager_and_complete_commands() {
  let home = Home::new();
  let (_, bytes) = archive('a');
  let provided = home.0.join("provided");
  fs::DirBuilder::new().mode(0o700).create(&provided).unwrap();
  tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()))
    .unpack(&provided)
    .unwrap();
  // Exercise the distributed executable form while retaining the production
  // size bound; Linux debug information alone can exceed that bound.
  let executable = home.0.join("ctlup-downloaded-target");
  fs::copy(env!("CARGO_BIN_EXE_ctlup"), &executable).unwrap();
  let stripped = Command::new("strip").arg(&executable).output().unwrap();
  assert!(stripped.status.success(), "{stripped:?}");
  let root = home.0.join(".tokn/ctl");
  let output = home
    .command_at(
      &executable,
      &["install", "--from", provided.to_str().unwrap(), "--json"],
    )
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let results: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(results[0]["error"].is_null());
  assert_eq!(results[0]["result"]["services_preserved"], true);
  let manager = root.join("bin/ctlup");
  assert!(fs::symlink_metadata(&manager).unwrap().is_file());
  let output = home
    .command_at(&manager, &["status", "--json"])
    .output()
    .unwrap();
  assert!(output.status.success());
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(!status["active"].is_null());
  assert!(!root.join("components/selected").exists());
}

#[test]
fn commands_use_one_complete_build_and_rollback_preserves_the_manager() {
  let home = Home::new();
  let (first, first_archive) = archive('a');
  let output = home.install(&first_archive);
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(String::from_utf8_lossy(&output.stdout).ends_with("ctl-install-v1\n"));
  let bin = home.0.join(".tokn/ctl/bin");
  fs::copy(env!("CARGO_BIN_EXE_ctlup"), bin.join("ctlup")).unwrap();
  fs::set_permissions(bin.join("ctlup"), fs::Permissions::from_mode(0o700)).unwrap();
  let manager_metadata = fs::metadata(bin.join("ctlup")).unwrap();
  for name in ctl_core::bundle_layout::ALL {
    let output = Command::new(bin.join(name)).output().unwrap();
    assert!(output.status.success());
    assert_eq!(
      String::from_utf8(output.stdout).unwrap(),
      format!("a-{name}\n")
    );
  }
  let (second, second_archive) = archive('b');
  let output = home.install(&second_archive);
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let status = home.command(&["status", "--json"]).output().unwrap();
  let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
  assert_eq!(value["active"]["bundle_id"], second.bundle_id);
  assert_eq!(value["previous"]["bundle_id"], first.bundle_id);
  let output = home.command(&["rollback", "--json"]).output().unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(value["active"]["bundle_id"], first.bundle_id);
  assert_eq!(value["previous"]["bundle_id"], second.bundle_id);
  assert_eq!(
    fs::metadata(bin.join("ctlup")).unwrap().len(),
    manager_metadata.len()
  );
  assert!(
    !fs::symlink_metadata(bin.join("ctlup"))
      .unwrap()
      .file_type()
      .is_symlink()
  );
  assert!(
    home
      .0
      .join(".tokn/ctl/installs")
      .join(first.bundle_id)
      .is_dir()
  );
  assert!(!home.0.join(".tokn/ctl/components/selected").exists());
}

#[test]
fn failed_archive_and_missing_preflight_do_not_change_the_active_installation() {
  let home = Home::new();
  let (first, bytes) = archive('a');
  assert!(home.install(&bytes).status.success());
  let previous = fs::read_link(home.0.join(".tokn/ctl/current")).unwrap();
  let output = home.install(b"corrupt archive");
  assert!(!output.status.success());
  assert_eq!(
    fs::read_link(home.0.join(".tokn/ctl/current")).unwrap(),
    previous
  );
  let output = home
    .command(&["activate-installed", &"b".repeat(64)])
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(10));
  let output = home
    .command(&["activate-installed", &first.bundle_id])
    .output()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(
    fs::read_link(home.0.join(".tokn/ctl/current")).unwrap(),
    previous
  );
}

#[cfg(target_os = "macos")]
fn development_payload() -> (Manifest, BTreeMap<String, Vec<u8>>) {
  let (components, mut files) = fixture_payload('b');
  let helper = &components["ctld"];
  let target = ctl_core::paths::native_target();
  let version = &helper.build.version;
  let archive_digest = "d".repeat(64);
  let executable = b"#!/bin/sh\ntouch \"$HOME/helper-executed\"\nexit 19\n".to_vec();
  files.insert("ctld".into(), executable.clone());
  files.insert("ctld.app/Contents/MacOS/ctld".into(), executable);
  let receipt = serde_json::json!({
    "schema_version": 1,
    "component": "ctld",
    "app_version": version,
    "bundle_id": format!("dev.{archive_digest}"),
    "git_revision": helper.build.source_revision,
    "target": target,
    "bundle_identifier": "dev.tokn-ai.ctl.ctld",
    "team_identifier": "ABCDEFGHIJ",
    "signing_mode": "development",
    "notarized": false,
    "archive": format!("ctld-{version}-{target}.app.tar.gz"),
    "sha256": archive_digest,
    "archive_size": 1,
    "development": {
      "source_fingerprint": helper.build.source_fingerprint,
      "dirty": helper.build.dirty,
    },
    "protocols": helper.protocols,
  });
  files.insert(
    "ctld-package.json".into(),
    serde_json::to_vec(&receipt).unwrap(),
  );
  let manifest = Manifest::new(target, Source::Local, components, &files).unwrap();
  (manifest, files)
}

#[cfg(target_os = "macos")]
#[test]
fn failed_renewal_passes_verified_signing_input_and_preserves_the_installation() {
  use ctl_core::bundles::Store;
  use ctl_core::installations::{CatalogOrigin, Installations};
  use sha2::{Digest as _, Sha256};

  let home = Home::new();
  let (previous, bytes) = archive('a');
  assert!(home.install(&bytes).status.success());
  let installations = Installations::new(&home.0);
  let (manifest, files) = development_payload();
  let pending = Store::cache(&home.0).publish(&manifest, &files).unwrap();
  let installed = installations
    .activate_with_origin(
      &pending,
      &CatalogOrigin {
        channel: "local".into(),
        publication_id: Some(format!(
          "local.{}",
          manifest.components["ctld"].build.source_fingerprint
        )),
        built_at: Some("2026-01-02T03:04:05Z".into()),
      },
    )
    .unwrap();
  let catalog_path = installations.root().join("catalog.json");
  let catalog = fs::read(&catalog_path).unwrap();

  let workspace = home.0.join("checkout with spaces");
  fs::create_dir_all(workspace.join("scripts/dev")).unwrap();
  fs::write(workspace.join("scripts/dev/ctld-signed.mts"), []).unwrap();
  let signer = home.0.join("signer");
  fs::write(
    &signer,
    r#"#!/bin/sh
set -eu
test "$#" -eq 5
test "$3" = "$(pwd -P)/scripts/dev/ctld-signed.mts"
test "$4" = --renew-signing
artifact=$(/usr/bin/plutil -extract artifact raw -o - "$5")
test -x "$artifact"
cp "$5" "$HOME/signing-input.json"
cp "$artifact" "$HOME/staged-ctld"
printf 'fixture signing failure\n' >&2
exit 17
"#,
  )
  .unwrap();
  fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
  let output = home
    .command(&["dev", "--renew-signing", "--json", "--workspace"])
    .arg(&workspace)
    .env("CTL_NODE_PROGRAM", &signer)
    .output()
    .unwrap();
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success());
  assert!(
    diagnostics.contains("fixture signing failure"),
    "{diagnostics}"
  );
  assert_eq!(output.stdout, [] as [u8; 0]);
  let request: serde_json::Value =
    serde_json::from_slice(&fs::read(home.0.join("signing-input.json")).unwrap()).unwrap();
  assert_eq!(request["target"], manifest.target_triple);
  assert_eq!(request["expected_team"], "ABCDEFGHIJ");
  assert_eq!(
    request["expected_component"],
    serde_json::to_value(&manifest.components["ctld"]).unwrap()
  );
  let helper = "ctld.app/Contents/MacOS/ctld";
  let artifact = PathBuf::from(request["artifact"].as_str().unwrap());
  assert_ne!(artifact, installed.directory.join(helper));
  assert_eq!(
    format!(
      "{:x}",
      Sha256::digest(fs::read(home.0.join("staged-ctld")).unwrap())
    ),
    manifest.files[helper].sha256
  );
  assert!(!home.0.join("helper-executed").exists());
  assert_eq!(installations.active().unwrap().unwrap().manifest, manifest);
  assert_eq!(
    installations.previous().unwrap().unwrap().manifest,
    previous
  );
  assert_eq!(fs::read(catalog_path).unwrap(), catalog);
  assert_eq!(installed.read_files().unwrap(), files);
}

#[test]
fn runtime_cli_redirects_installation_commands_without_writing_files() {
  let home = Home::new();
  let output = Command::new(env!("CARGO_BIN_EXE_ctl"))
    .args(["components", "update", "--from", "/missing-build"])
    .env("HOME", &home.0)
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert!(
    String::from_utf8_lossy(&output.stderr)
      .to_lowercase()
      .contains("use ctlup")
  );
  assert!(!home.0.join(".tokn").exists());
  let help = home.command(&["update", "--help"]).output().unwrap();
  let help = String::from_utf8(help.stdout).unwrap();
  for removed in ["--package", "--selected", "--purpose"] {
    assert!(!help.contains(removed));
  }
}
