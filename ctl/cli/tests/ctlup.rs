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

fn archive(revision: char) -> (Manifest, Vec<u8>) {
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
  let files: BTreeMap<String, Vec<u8>> = ctl_core::bundle_layout::ALL
    .into_iter()
    .map(|name| {
      (
        name.to_owned(),
        format!("#!/bin/sh\nprintf '%s\\n' '{revision}-{name}'\n").into_bytes(),
      )
    })
    .collect();
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
  let manager = home.0.join(".tokn/ctl/bin/ctlup");
  assert!(fs::symlink_metadata(&manager).unwrap().is_file());
  let output = Command::new(manager)
    .args(["status", "--json"])
    .env("HOME", &home.0)
    .output()
    .unwrap();
  assert!(output.status.success());
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(!status["active"].is_null());
  assert!(!home.0.join(".tokn/ctl/components/selected").exists());
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
