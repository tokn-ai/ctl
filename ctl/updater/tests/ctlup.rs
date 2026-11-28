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
    command.env_remove("CTL_DATA_DIR");
    command.env_remove("CTL_CONFIG_DIR");
    command
  }

  fn command_manager(&self, args: &[&str]) -> Command {
    let binary = Path::new(env!("CARGO_BIN_EXE_ctlup"));
    let limit = ctl_core::bundles::MAX_FILE_BYTES as u64;
    if fs::metadata(binary).unwrap().len() <= limit {
      return self.command_at(binary, args);
    }
    let stripped = self.0.join("ctlup-test-manager");
    if !stripped.exists() {
      fs::copy(binary, &stripped).unwrap();
      let result = Command::new("strip").arg(&stripped).output().unwrap();
      assert!(result.status.success(), "{result:?}");
    }
    self.command_at(&stripped, args)
  }

  fn updater_root(&self) -> PathBuf {
    ctl_core::paths::updater_directory_for_account(&self.0)
  }

  fn registry(&self) -> PathBuf {
    self.updater_root().join("state/installations.json")
  }

  fn payloads(&self) -> PathBuf {
    self.updater_root().join("store/runtime")
  }

  fn payload(&self, bundle_id: &str) -> PathBuf {
    ctl_core::installations::Installations::new(&self.0)
      .get(bundle_id)
      .unwrap()
      .directory
  }

  fn install(&self, archive: &[u8]) -> Output {
    let mut child = self
      .command_manager(&["install-archive"])
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
fn inspection_leaves_an_unrelated_legacy_installation_intact() {
  let home = Home::new();
  let profile = home.0.join(".tokn/ctl");
  let legacy = profile.join("installs/legacy");
  fs::create_dir_all(&legacy).unwrap();
  fs::write(legacy.join("payload"), b"old content").unwrap();
  std::os::unix::fs::symlink("installs/legacy", profile.join("current")).unwrap();

  let preparation = home.command(&["prepare-install"]).output().unwrap();
  assert!(preparation.status.success(), "{preparation:?}");
  assert_eq!(preparation.stdout, b"");
  let status = home.command(&["status", "--json"]).output().unwrap();
  assert!(status.status.success(), "{status:?}");
  let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
  assert!(status["active"].is_null());
  assert_eq!(status["installations"], serde_json::json!([]));
  assert!(!home.registry().exists());
  assert_eq!(fs::read(legacy.join("payload")).unwrap(), b"old content");
}

#[test]
fn runtime_preflight_reads_the_registry_without_replacing_the_selection() {
  let home = Home::new();
  let (active, bytes) = archive('a');
  let installed = home.install(&bytes);
  assert!(installed.status.success(), "{installed:?}");
  let recorded = fs::read(home.registry()).unwrap();
  let output = home.command(&["prepare-install"]).output().unwrap();
  assert!(output.status.success(), "{output:?}");
  assert_eq!(output.stdout, b"");
  assert_eq!(fs::read(home.registry()).unwrap(), recorded);
  let installed = ctl_core::installations::Installations::new(&home.0)
    .active()
    .unwrap()
    .unwrap();
  assert_eq!(installed.manifest, active);
  assert_eq!(
    installed.directory.file_name().unwrap(),
    active.target_triple.as_str()
  );
  assert_eq!(
    installed.directory.ancestors().nth(2).unwrap(),
    home.payloads()
  );
  assert_eq!(installed.directory, home.payload(&active.bundle_id));
}

#[test]
fn update_checks_pinned_and_uninstalled_sources_without_resolving_a_release() {
  let home = Home::new();
  let check = || {
    let output = home
      .command(&["update", "--check", "--json"])
      .output()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
  };
  assert_eq!(check()[0]["check"]["state"], "uninstalled");
  assert!(!home.registry().exists());
  let (manifest, bytes) = archive('a');
  let installed = home.install(&bytes);
  assert!(installed.status.success(), "{installed:?}");
  assert_eq!(check()[0]["check"]["state"], "pinned");
  assert_eq!(check()[0]["check"]["active"], manifest.bundle_id);

  let before = fs::read(home.registry()).unwrap();
  let update = home.command(&["update"]).output().unwrap();
  assert!(update.status.success(), "{update:?}");
  assert!(String::from_utf8_lossy(&update.stdout).contains("runtime is pinned"));
  assert_eq!(fs::read(home.registry()).unwrap(), before);
}

#[test]
fn standalone_migration_command_is_removed() {
  let home = Home::new();
  let output = home.command(&["migrate"]).output().unwrap();
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand 'migrate'"));
  assert!(!home.0.join(".tokn").exists());
}

#[test]
fn status_reports_missing_installed_bundles_and_archive_install_restores_them() {
  let home = Home::new();
  let (manifest, bytes) = archive('a');
  let installed = home.install(&bytes);
  assert!(installed.status.success(), "{installed:?}");
  let catalog = fs::read(home.registry()).unwrap();
  let payload = home.payload(&manifest.bundle_id);
  fs::rename(home.payloads(), home.0.join("saved-runtime")).unwrap();

  let output = home.command(&["status", "--json"]).output().unwrap();
  assert!(output.status.success(), "{output:?}");
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(status["active"], manifest.bundle_id);
  assert_eq!(status["installations"][0]["bundle_id"], manifest.bundle_id);
  assert_eq!(
    status["missing_builds"],
    serde_json::json!([manifest.bundle_id])
  );
  assert_eq!(status["builds"], serde_json::json!([]));
  let human = home.command(&["status"]).output().unwrap();
  assert!(human.status.success(), "{human:?}");
  let text = String::from_utf8(human.stdout).unwrap();
  assert!(
    text.contains(&format!("Active: {} (missing)", &manifest.bundle_id[..12])),
    "{text}"
  );

  let inspection = home
    .command(&["inspect-runtime", "--json"])
    .output()
    .unwrap();
  assert!(inspection.status.success(), "{inspection:?}");
  let inspection: serde_json::Value = serde_json::from_slice(&inspection.stdout).unwrap();
  assert!(inspection["active"].is_null());
  assert_eq!(inspection["missing_builds"], status["missing_builds"]);
  assert_eq!(fs::read(home.registry()).unwrap(), catalog);
  assert!(!home.payloads().exists());

  let probe = home
    .command(&["activate-installed", &manifest.bundle_id])
    .output()
    .unwrap();
  assert_eq!(probe.status.code(), Some(10), "{probe:?}");
  let recovered = home.install(&bytes);
  assert!(recovered.status.success(), "{recovered:?}");
  assert!(payload.is_dir());
  let output = home.command(&["status", "--json"]).output().unwrap();
  assert!(output.status.success(), "{output:?}");
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(status["active"], manifest.bundle_id);
  assert!(status.get("missing_builds").is_none());
}

#[test]
fn prune_forgets_missing_records_only_when_requested() {
  let home = Home::new();
  let (manifest, bytes) = archive('a');
  let installed = home.install(&bytes);
  assert!(installed.status.success(), "{installed:?}");
  fs::remove_dir_all(home.payloads()).unwrap();

  let prune = |args: &[&str]| {
    let output = home.command(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
  };
  let pruned = prune(&["prune", "--json"]);
  assert_eq!(
    pruned["installations"]["missing_builds"],
    serde_json::json!([manifest.bundle_id])
  );
  let pruned = prune(&["prune", "--forget-missing", "--json"]);
  assert_eq!(
    pruned["installations"]["forgotten_missing_builds"],
    serde_json::json!([manifest.bundle_id])
  );
  assert_eq!(pruned["installations"]["removed"], serde_json::json!([]));
  assert!(!home.payloads().exists());

  let status = home.command(&["status", "--json"]).output().unwrap();
  assert!(status.status.success(), "{status:?}");
  let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
  assert!(status["active"].is_null());
  assert!(status.get("missing_builds").is_none());
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
    "scripts/dev/build-local-development.mts",
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
  assert!(!text.contains("ctlup dev"));

  let output = home
    .command(&["status"])
    .current_dir(&checkout)
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
    .command_manager(&["install-archive"])
    .env("CTL_DATA_DIR", &root)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(&bytes).unwrap();
  let output = child.wait_with_output().unwrap();
  assert!(output.status.success(), "{output:?}");
  let updater_root = root.join("ctlup");
  let mut builds = fs::read_dir(updater_root.join("store/runtime")).unwrap();
  let build = builds.next().unwrap().unwrap().path();
  assert!(builds.next().is_none());
  let name = build.file_name().unwrap().to_str().unwrap();
  assert!(name.starts_with("provided-"), "{name}");
  assert!(name.contains("-v0.1.2-"), "{name}");
  let payload = build.join(&manifest.target_triple);
  assert_eq!(
    serde_json::from_slice::<Manifest>(&fs::read(payload.join(MANIFEST_FILE)).unwrap()).unwrap(),
    manifest
  );
  assert!(updater_root.join("state/installations.json").is_file());
  assert!(!updater_root.join("refs").exists());
  assert!(root.join("bin/ctl").is_file());
  let output = home
    .command(&["status", "--json"])
    .env("CTL_DATA_DIR", &root)
    .output()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(!status["active"].is_null());
  let pruned = home
    .command(&["prune", "--json"])
    .env("CTL_DATA_DIR", &root)
    .output()
    .unwrap();
  assert!(pruned.status.success(), "{pruned:?}");
  assert!(payload.is_dir());
  assert!(!home.0.join(".tokn").exists());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn ctream_preview_uses_the_selected_profiles_managed_ctl() {
  use ctl_bundle::ctream::{agent_paths, archive_name, native_target, viewer_executables};

  let home = Home::new();
  let profile = home.0.join("alternate profile");
  fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
  let (_, bytes) = archive('a');
  let mut child = home
    .command_manager(&["install-archive"])
    .env("CTL_DATA_DIR", &profile)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(&bytes).unwrap();
  let runtime = child.wait_with_output().unwrap();
  assert!(runtime.status.success(), "{runtime:?}");

  let bundle = home.0.join("fixture.zip");
  let target = native_target().unwrap();
  let folder = archive_name(target);
  let viewers = viewer_executables(target);
  let mut writer = zip::ZipWriter::new(fs::File::create(&bundle).unwrap());
  for name in viewers.into_iter().map(str::to_owned).chain(agent_paths()) {
    writer
      .start_file(
        format!("{folder}/{name}"),
        zip::write::SimpleFileOptions::default(),
      )
      .unwrap();
    let payload: &[u8] = if name == viewers[0] {
      b"#!/bin/sh\nset -e\n\"$@\"\nexec ctl host list --json\n"
    } else {
      b"#!/bin/sh\nexit 0\n"
    };
    writer.write_all(payload).unwrap();
  }
  writer.finish().unwrap();
  let installed = home
    .command(&["ctream", "install", "--from"])
    .arg(bundle)
    .env("CTL_DATA_DIR", &profile)
    .output()
    .unwrap();
  assert!(installed.status.success(), "{installed:?}");

  let inherited = home.0.join("inherited tools");
  fs::create_dir(&inherited).unwrap();
  let add_tool = |name, script: &[u8]| {
    let path = inherited.join(name);
    fs::write(&path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
  };
  add_tool("ctream-test-tool", b"#!/bin/sh\nprintf '%s\\n' inherited\n");
  let run_preview = |search_path: Option<&Path>, args: &[&str], tool_output: &str| {
    let mut command = home.command_at(&profile.join("bin/ctlup"), &["ctream", "run", "--"]);
    command.args(args).env("CTL_DATA_DIR", &profile);
    if let Some(search_path) = search_path {
      command.env("PATH", search_path);
    } else {
      command.env_remove("PATH");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, format!("{tool_output}\na-ctl\n").as_bytes());
  };
  run_preview(Some(&inherited), &["ctream-test-tool"], "inherited");
  add_tool("ctl", b"#!/bin/sh\nprintf '%s\\n' unmanaged-ctl\n");
  run_preview(Some(&inherited), &["ctream-test-tool"], "inherited");
  run_preview(None, &["sh", "-c", "printf '%s\\n' system"], "system");
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
  if fs::metadata(&executable).unwrap().len() > ctl_core::bundles::MAX_FILE_BYTES as u64 {
    let stripped = Command::new("strip").arg(&executable).output().unwrap();
    assert!(stripped.status.success(), "{stripped:?}");
  }
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
  assert!(manager.is_file());
  let output = home
    .command_at(&manager, &["status", "--json"])
    .output()
    .unwrap();
  assert!(output.status.success());
  let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(!status["active"].is_null());
  assert!(!root.join("components/selected").exists());

  let active = status["active"].as_str().unwrap();
  let payload = home.payload(active);
  fs::remove_dir_all(&payload).unwrap();
  let repair = home
    .command_at(&manager, &["update", "--json"])
    .output()
    .unwrap();
  assert!(repair.status.success(), "{repair:?}");
  assert!(payload.is_dir());
  let status = home
    .command_at(&manager, &["status", "--json"])
    .output()
    .unwrap();
  let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
  assert_eq!(status["active"], active);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn dev_rebuilds_the_current_checkout_when_its_payload_is_missing() {
  let home = Home::new();
  let workspace = home.0.join("workspace");
  fs::create_dir_all(workspace.join("scripts/dev")).unwrap();
  fs::write(workspace.join("Cargo.toml"), "fixture").unwrap();
  fs::write(
    workspace.join("scripts/dev/build-local-development.mts"),
    "fixture",
  )
  .unwrap();
  let bin = workspace.join("bin");
  fs::create_dir(&bin).unwrap();
  let layout_version = ctl_core::installations::DEVELOPMENT_LAYOUT_VERSION;
  let write_components = |revision| {
    let (info, _) = fixture_payload(revision);
    for name in ctl_core::bundle_layout::ALL.into_iter().chain(["ctlup"]) {
      let metadata = serde_json::to_string(&info["ctl"]).unwrap();
      let executable = bin.join(name);
      fs::write(&executable, format!(
        "#!/bin/sh\ncase \"$1\" in\n--component-info) printf '%s\\n' '{metadata}';;\n--version) printf '%s\\n' '{name} 0.1.2';;\ninstallation-layout-version) printf '%s\\n' '{layout_version}';;\n*) exit 1;;\nesac\n"
      )).unwrap();
      fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
  };
  let builder = workspace.join("builder");
  fs::write(&builder, r#"#!/bin/sh
set -eu
umask 077
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) output=$2; shift ;;
    --native-target) target=$2; shift ;;
  esac
  shift
done
mkdir "$output/bin"
cp bin/* "$output/bin/"
printf '{"schema_version":1,"binary_directory":"%s/bin","manager_executable":"%s/bin/ctlup","target_triple":"%s","ctld_package":null}\n' "$output" "$output" "$target" > "$output/result.json"
"#).unwrap();
  fs::set_permissions(&builder, fs::Permissions::from_mode(0o700)).unwrap();
  let dev = || {
    let output = home
      .command(&["dev", "--workspace", workspace.to_str().unwrap(), "--json"])
      .env("CTL_NODE_PROGRAM", &builder)
      .output()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
  };
  write_components('a');
  let first = dev();
  fs::remove_dir_all(home.payloads()).unwrap();
  write_components('b');
  let rebuilt = dev();
  assert_ne!(first["bundle_id"], rebuilt["bundle_id"]);
  assert!(Path::new(rebuilt["executables"]["ctl"].as_str().unwrap()).is_file());
  assert_eq!(
    ctl_core::installations::Installations::new(&home.0)
      .active_selection()
      .unwrap()
      .0
      .as_deref(),
    rebuilt["bundle_id"].as_str(),
  );
  let payload = home.payload(rebuilt["bundle_id"].as_str().unwrap());
  assert_eq!(
    payload.file_name().unwrap(),
    ctl_core::paths::native_target()
  );
  let name = payload
    .parent()
    .unwrap()
    .file_name()
    .unwrap()
    .to_str()
    .unwrap();
  assert!(name.starts_with("dev-"), "{name}");
  assert!(name.contains("-v0.1.2-"), "{name}");
  assert!(name.ends_with("-local"), "{name}");
  assert!(!home.updater_root().join("refs").exists());
}

#[test]
fn commands_switch_complete_builds_and_prune_old_payloads_after_leases_end() {
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
  let manager_metadata = fs::metadata(bin.join("ctlup")).unwrap();
  let assert_commands = |revision: char| {
    for name in ctl_core::bundle_layout::ALL {
      let output = home.command_at(&bin.join(name), &[]).output().unwrap();
      assert!(output.status.success());
      assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("{revision}-{name}\n")
      );
    }
  };
  assert_commands('a');
  let installations = ctl_core::installations::Installations::new(&home.0);
  let leased = installations.active().unwrap().unwrap();
  let lease = installations.lease(&leased).unwrap();
  assert_eq!(leased.manifest, first);
  let (second, second_archive) = archive('b');
  let output = home.install(&second_archive);
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let status = home.command(&["status", "--json"]).output().unwrap();
  let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
  assert_eq!(value["active"], second.bundle_id);
  assert!(value["previous"].is_null());
  assert_commands('b');
  assert_eq!(
    fs::metadata(bin.join("ctlup")).unwrap().len(),
    manager_metadata.len()
  );
  assert_eq!(
    fs::read_link(bin.join("ctlup")).unwrap(),
    Path::new(".ctl-launcher")
  );
  assert!(leased.directory.is_dir());
  drop(lease);
  let pruned = home.command(&["prune", "--json"]).output().unwrap();
  assert!(
    pruned.status.success(),
    "{}",
    String::from_utf8_lossy(&pruned.stderr)
  );
  assert!(!leased.directory.exists());
  assert!(!home.0.join(".tokn/ctl/components/selected").exists());
}

#[test]
fn failed_archive_and_missing_preflight_do_not_change_the_active_installation() {
  let home = Home::new();
  let (first, bytes) = archive('a');
  assert!(home.install(&bytes).status.success());
  let installations = ctl_core::installations::Installations::new(&home.0);
  let before = installations.active_selection().unwrap().0;
  let output = home.install(b"corrupt archive");
  assert!(!output.status.success());
  assert_eq!(installations.active_selection().unwrap().0, before);
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
  assert_eq!(installations.active_selection().unwrap().0, before);
}

#[test]
fn standalone_signing_replaces_the_development_subcommand() {
  let home = Home::new();
  let help = home.command(&["sign", "--help"]).output().unwrap();
  assert!(help.status.success());
  let help = String::from_utf8(help.stdout).unwrap();
  assert!(help.contains("Usage: ctlup sign [OPTIONS]"));
  assert!(help.contains("--version <VERSION>"));
  assert!(help.contains("--channel <CHANNEL>"));

  let obsolete = home.command(&["dev", "sign"]).output().unwrap();
  assert!(!obsolete.status.success());
  assert!(String::from_utf8_lossy(&obsolete.stderr).contains("unexpected argument 'sign'"));
  assert!(!home.registry().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn signing_without_a_cached_desktop_explains_local_preparation() {
  let home = Home::new();
  let output = home.command(&["sign"]).output().unwrap();
  assert!(!output.status.success());
  let message = String::from_utf8(output.stderr).unwrap();
  assert!(
    message.contains("No active local development runtime or cached desktop build"),
    "{message}"
  );
  assert!(message.contains("ctlup install app"), "{message}");
  assert!(message.contains("--version"), "{message}");
  assert!(!home.registry().exists());
}

#[cfg(target_os = "macos")]
#[test]
fn team_setup_reaches_provisioning_without_a_build() {
  let home = Home::new();
  let settings = home.updater_root().join("signing");
  fs::create_dir_all(&settings).unwrap();
  let preference = settings.join("provisioning-team");
  fs::write(&preference, b"invalid\n").unwrap();
  fs::set_permissions(&preference, fs::Permissions::from_mode(0o600)).unwrap();

  let output = home
    .command(&["sign", "--provision", "--team", "--json"])
    .output()
    .unwrap();
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success());
  assert!(
    diagnostics.contains("invalid saved Apple Team selection"),
    "{diagnostics}"
  );
  assert_eq!(fs::read(preference).unwrap(), b"invalid\n");
  assert!(!home.registry().exists());
  assert!(!home.updater_root().join("cache/downloads/desktop").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn signing_without_a_desktop_preserves_an_active_runtime_installation() {
  let home = Home::new();
  let (_, bytes) = archive('a');
  let output = home.install(&bytes);
  assert!(output.status.success(), "{output:?}");
  let recorded = fs::read(home.registry()).unwrap();
  let output = home.command(&["sign"]).output().unwrap();
  assert!(!output.status.success());
  let message = String::from_utf8(output.stderr).unwrap();
  assert!(message.contains("cached desktop build"), "{message}");
  assert!(message.contains("ctlup install app"), "{message}");
  assert_eq!(fs::read(home.registry()).unwrap(), recorded);
}

#[cfg(target_os = "macos")]
#[test]
fn signing_selectors_require_a_local_cached_package_before_credentials_or_network() {
  for (options, expected) in [
    (
      &["--version", "v0.1.4"][..],
      "No desktop package for 0.1.4 is cached",
    ),
    (
      &["--channel", "nightly"][..],
      "No nightly desktop package is cached",
    ),
    (
      &["--channel", "stable"][..],
      "No stable desktop package is cached",
    ),
  ] {
    let home = Home::new();
    let output = home
      .command(&["sign"])
      .args(options)
      .env_remove("CTLD_PROVISIONING_PROFILE")
      .env_remove("APPLE_API_KEY_PATH")
      .env_remove("APPLE_API_KEY")
      .env_remove("APPLE_API_ISSUER")
      .env("HTTPS_PROXY", "http://127.0.0.1:9")
      .env("https_proxy", "http://127.0.0.1:9")
      .env("NO_PROXY", "")
      .env("no_proxy", "")
      .output()
      .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    assert!(message.contains(expected), "{message}");
    assert!(message.contains("Install"), "{message}");
    assert!(
      !message.contains("Set CTLD_PROVISIONING_PROFILE"),
      "{message}"
    );
    assert!(!message.contains("Downloading"), "{message}");
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(!home.registry().exists());
    assert!(!home.updater_root().join("signed").exists());
  }
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
fn development_signing_uses_native_provisioning_without_a_checkout() {
  use ctl_core::bundles::Store;
  use ctl_core::installations::{CatalogOrigin, Installations};

  let home = Home::new();
  let (_, bytes) = archive('a');
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
  let catalog_path = home.registry();
  let catalog = fs::read(&catalog_path).unwrap();

  let signer = home.0.join("signer");
  fs::write(
    &signer,
    r#"#!/bin/sh
touch "$HOME/node-executed"
exit 17
"#,
  )
  .unwrap();
  fs::set_permissions(&signer, fs::Permissions::from_mode(0o700)).unwrap();
  let output = home
    .command(&["sign", "--json"])
    .current_dir(&home.0)
    .env("CTL_NODE_PROGRAM", &signer)
    .env(
      "CTLD_PROVISIONING_PROFILE",
      home.0.join("missing.provisionprofile"),
    )
    .env_remove("APPLE_API_KEY_PATH")
    .env_remove("APPLE_API_KEY")
    .env_remove("APPLE_API_ISSUER")
    .output()
    .unwrap();
  let diagnostics = String::from_utf8_lossy(&output.stderr);
  assert!(!output.status.success());
  assert!(!diagnostics.contains("ctl checkout root"), "{diagnostics}");
  assert_eq!(output.stdout, [] as [u8; 0]);
  assert!(!home.0.join("node-executed").exists());
  assert!(!home.0.join("helper-executed").exists());
  assert_eq!(installations.active().unwrap().unwrap().manifest, manifest);
  assert!(installations.previous().unwrap().is_none());
  assert_eq!(fs::read(catalog_path).unwrap(), catalog);
  assert_eq!(installed.read_files().unwrap(), files);
}

#[test]
fn update_help_omits_removed_options() {
  let home = Home::new();
  let help = home.command(&["update", "--help"]).output().unwrap();
  assert!(help.status.success());
  let help = String::from_utf8(help.stdout).unwrap();
  for removed in ["--package", "--selected", "--purpose"] {
    assert!(!help.contains(removed));
  }
}
