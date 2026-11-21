#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use ctl_ipc::{ClientMessage, ServerMessage};
use serde_json::json;
use sha2::{Digest as _, Sha256};

fn fixture_target() -> &'static str {
  match (std::env::consts::OS, std::env::consts::ARCH) {
    ("macos", "aarch64") => "aarch64-apple-darwin",
    ("macos", "x86_64") => "x86_64-apple-darwin",
    (_, "aarch64") => "aarch64-unknown-linux-musl",
    _ => "x86_64-unknown-linux-musl",
  }
}

const EXPECTED_ID: &str = "fa5f874a-bd3c-4a41-9d8f-d584bd9c9a14";

#[path = "remote_repair/components_update.rs"]
mod components_update;

struct Fixture(PathBuf);

impl Fixture {
  fn new(observed_id: &str) -> Self {
    // Keep Unix socket paths below the macOS path-length limit.
    let directory = PathBuf::from("/tmp").join(format!("ctl-repair-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Self(directory);
    fs::create_dir_all(fixture.0.join("home/.tokn/ctl")).unwrap();
    fs::write(fixture.0.join("home/.tokn/ctl/remote-id"), observed_id).unwrap();
    fs::write(
      fixture.0.join("hosts.json"),
      serde_json::to_vec(&json!({
        "revision": "test", "document": {"schema_version": 1, "ssh_gateways": [], "hosts": [
          {"host_id": "host-1", "name": "work", "preferred_method_id": "direct",
            "remote_info": {"remote_id": EXPECTED_ID, "agent_version": "0.1.0"},
            "connection_methods": [
              {"method_id": "direct", "name": "Direct", "target": {
                "kind": "ssh", "destination": "10.0.0.20", "user": "alice", "port": 2222
              }}
            ]}
        ]}
      }))
      .unwrap(),
    )
    .unwrap();
    fs::create_dir(fixture.0.join("bundles")).unwrap();
    fs::write(
      fixture.0.join("bundles/bundle-set.json"),
      b"invalid fixture manifest",
    )
    .unwrap();
    fixture.script(
      "ssh",
      r#"
count=0
if [ -f "$CTL_TEST_DIR/count" ]; then read -r count < "$CTL_TEST_DIR/count"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$CTL_TEST_DIR/count"
printf '%s\n' "$@" > "$CTL_TEST_DIR/ssh-$count.args"
last=''
for argument do last=$argument; done
case "$last" in
  *remote-id*)
    touch "$CTL_TEST_DIR/account-id"
    exec /bin/sh -c "$last"
    ;;
  *'exec ctl-agent inspect')
    touch "$CTL_TEST_DIR/inspect"
    exec /bin/sh -c "$last"
    ;;
  *ctl-platform-v1*)
    touch "$CTL_TEST_DIR/platform"
    printf 'ctl-platform-v1\n'
    uname -s
    uname -m
    ;;
  *ctlup-bootstrap-v1*)
    touch "$CTL_TEST_DIR/bootstrap"
    exec /bin/sh -c "$last"
    ;;
  *ctlup-command-v1*|*ctlup-runtime-source-v1*)
    exec /bin/sh -c "$last"
    ;;
  *activate-installed*)
    touch "$CTL_TEST_DIR/preflight"
    exec /bin/sh -c "$last"
    ;;
  *install-archive*|*ctl-install-progress-v1*)
    touch "$CTL_TEST_DIR/upload"
    if [ -f "$CTL_TEST_DIR/allow-upload" ]; then
      exec /bin/sh -c "$last"
    fi
    printf 'unexpected upload in fixture\n' >&2
    exit 91
    ;;
  *) printf 'unexpected SSH operation\n' >&2; exit 92 ;;
esac
"#,
    );
    fixture.script("gh", "touch \"$CTL_TEST_DIR/gh-called\"; exit 93");
    fixture.script(
      "node",
      "touch \"$CTL_TEST_DIR/builder-called\"; printf 'current development builder unavailable in fixture\\n' >&2; exit 94",
    );
    fixture
  }

  fn prepare_updater(&self) {
    let executable = self.0.join("ctlup-fixture-manager");
    fs::copy(env!("CARGO_BIN_EXE_ctlup"), &executable).unwrap();
    // Match the shipped form instead of relaxing the 128 MiB input limit for
    // unstripped Linux debug information.
    let stripped = Command::new("strip").arg(&executable).output().unwrap();
    assert!(stripped.status.success(), "{stripped:?}");
    fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
  }

  fn compatible_bundle(&self, build: &ctl_core::component::ComponentBuildInfo) -> String {
    self.bundle(build, &Self::compatible_components(build))
  }

  fn compatible_components(build: &ctl_core::component::ComponentBuildInfo) -> serde_json::Value {
    let protocol = |name: &str, build: u16| {
      let version = format!("1.0.{build}");
      json!({"name": name, "build": build, "version": version, "supported_versions": [version]})
    };
    let component = |protocols| json!({"build": build, "protocols": protocols});
    let ctld_protocols: Vec<serde_json::Value> = ctl_ipc::lifecycle::DaemonBinaryInfo::current()
      .protocols
      .into_iter()
      .map(|protocol| json!(protocol))
      .collect();
    let consumed_ctld = ctld_protocols
      .iter()
      .find(|protocol| protocol["name"] == "ctld")
      .unwrap();
    let mut components = json!({
      "ctl-agent": component(vec![
        protocol("ctl_identity", 3), protocol("ctl_maintenance", 2),
        protocol("ctl_remote_vpn", 1), consumed_ctld.clone(),
        protocol("ctl_transport", 1),
        protocol("ctmux", 13), protocol("ctmux_control", 1),
        protocol("task", 4), protocol("task_control", 2),
      ]),
      "ctmuxd": component(vec![protocol("ctmux", 13), protocol("ctmux_control", 1)]),
      "ctl-taskd": component(vec![
        protocol("task", 4), protocol("task_control", 2),
        protocol("ctmux", 13), protocol("ctmux_control", 1),
      ]),
      "ctld": component(ctld_protocols),
    });
    components["ctl"] = components["ctl-agent"].clone();
    let consumed_contracts = components["ctl"]["protocols"].as_array_mut().unwrap();
    for protocol in ctl_ipc::lifecycle::DaemonBinaryInfo::current().protocols {
      let protocol = json!(protocol);
      // The client consumes the current broker contract even when the fixture
      // agent advertises an older compatible version of the same protocol.
      consumed_contracts.retain(|existing| existing["name"] != protocol["name"]);
      consumed_contracts.push(protocol);
    }
    components["ctmux"] = components["ctmuxd"].clone();
    components
  }

  fn stale_sources(&self) -> ctl_core::bundles::Bundle {
    let previous = Self::new(EXPECTED_ID);
    let build = ctl_core::component::ComponentBuildInfo {
      version: "0.0.8".into(),
      source_revision: Some("b".repeat(40)),
      source_fingerprint: "b".repeat(64),
      dirty: false,
    };
    previous.compatible_bundle(&build);
    let target = fixture_target();
    let candidate = ctl_client::remote_bundle::read_verified_bundle(
      &[previous.0.join("bundles")],
      target,
      &build,
    )
    .unwrap()
    .unwrap();
    let home = self.0.join("home");
    ctl_client::remote_bundle::BundleCacheEntry::new(
      &home.join(".tokn/ctl/ctlup/agent-bundles"),
      target,
      &build,
    )
    .unwrap()
    .store(&candidate)
    .unwrap();
    let bundle = ctl_client::components::import_remote(
      &home,
      &candidate,
      target,
      ctl_core::bundles::Source::Ci,
    )
    .unwrap();
    // Recreate a pre-ctlup selection, rather than asking the deployment cache
    // to create an upload profile in the new installation model.
    let bundle = ctl_core::bundles::Store::new(&home)
      .publish(&bundle.manifest, &bundle.read_files().unwrap())
      .unwrap();
    ctl_core::bundles::Store::new(&home)
      .select(ctl_core::bundles::Purpose::Upload, &bundle)
      .unwrap();
    bundle
  }

  fn bundle(
    &self,
    build: &ctl_core::component::ComponentBuildInfo,
    components: &serde_json::Value,
  ) -> String {
    let revision = build.source_revision.as_deref().unwrap();
    let bundle_id = format!("{}-dev.{}", build.version, &revision[..12]);
    let target = fixture_target();
    let binaries = ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "ctl", "ctmux"];
    self.write_agent_identity(
      build,
      &bundle_id,
      target,
      &components["ctl-agent"]["protocols"],
    );
    let payload = self.0.join("payload");
    fs::create_dir(&payload).unwrap();
    fs::write(
      payload.join("ctl-agent"),
      b"#!/bin/sh\nset -eu\n\
         if [ \"$1\" = inspect ]; then exec cat \"$CTL_TEST_DIR/upgraded-identity\"; fi\n\
         printf 'unexpected direct service command after broker handoff\\n' >&2\nexit 95\n",
    )
    .unwrap();
    for binary in binaries.iter().filter(|binary| **binary != "ctl-agent") {
      fs::write(payload.join(binary), b"#!/bin/sh\nexit 0\n").unwrap();
    }
    let mut files = serde_json::Map::new();
    for &binary in &binaries {
      fs::set_permissions(payload.join(binary), fs::Permissions::from_mode(0o700)).unwrap();
      files.insert(
        binary.into(),
        json!(format!(
          "{:x}",
          Sha256::digest(fs::read(payload.join(binary)).unwrap())
        )),
      );
    }
    fs::write(
      payload.join("manifest.json"),
      serde_json::to_vec(&json!({
        "schema_version": 3, "app_version": build.version, "bundle_id": bundle_id,
        "git_revision": revision, "target_triple": target,
        "files": files, "components": components,
      }))
      .unwrap(),
    )
    .unwrap();
    let checksum = self.package_payload(&payload, &bundle_id, target, &binaries);
    let mut targets = serde_json::Map::new();
    for target in [
      "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-musl",
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
    ] {
      let entry = json!({
        "archive": format!("ctl-agent-bundle-{bundle_id}-{target}.tar.gz"),
        "sha256": checksum,
        "components": components,
      });
      targets.insert(target.into(), entry);
    }
    fs::write(
      self.0.join("bundles/bundle-set.json"),
      serde_json::to_vec(&json!({
        "schema_version": 3, "app_version": build.version,
        "bundle_id": bundle_id, "git_revision": revision, "targets": targets,
      }))
      .unwrap(),
    )
    .unwrap();
    fs::create_dir_all(self.0.join("home/.tokn/ctl")).unwrap();
    fs::write(self.0.join("home/.tokn/ctl/remote-id"), EXPECTED_ID).unwrap();
    fs::write(self.0.join("allow-upload"), []).unwrap();
    bundle_id
  }

  fn package_payload(
    &self,
    payload: &std::path::Path,
    bundle_id: &str,
    target: &str,
    binaries: &[&str],
  ) -> String {
    let archive = self.0.join(format!(
      "bundles/ctl-agent-bundle-{bundle_id}-{target}.tar.gz"
    ));
    let output = Command::new("tar")
      .env("COPYFILE_DISABLE", "1")
      .args(["--format=ustar", "-czf"])
      .arg(&archive)
      .arg("-C")
      .arg(payload)
      .args(binaries)
      .arg("manifest.json")
      .output()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    format!("{:x}", Sha256::digest(fs::read(&archive).unwrap()))
  }

  fn write_agent_identity(
    &self,
    build: &ctl_core::component::ComponentBuildInfo,
    bundle_id: &str,
    target: &str,
    protocols: &serde_json::Value,
  ) {
    let identity = json!({
      "remote_id": EXPECTED_ID,
      "protocols": protocols,
      "agent_version": build.version,
      "build": build,
      "ctmux_restart_supported": true,
      "bundle": {
        "app_version": build.version,
        "bundle_id": bundle_id,
        "git_revision": build.source_revision,
        "target_triple": target,
      },
    });
    fs::write(
      self.0.join("upgraded-identity"),
      serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
  }

  fn script(&self, name: &str, body: &str) {
    let path = self.0.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
  }

  fn updater_broker(&self) -> tokio::task::JoinHandle<ctl_ipc::SshTarget> {
    let listener = tokio::net::UnixListener::bind(self.0.join("ctld.sock")).unwrap();
    let control_path = self.0.join("master");
    tokio::spawn(async move {
      tokio::time::timeout(Duration::from_secs(20), async {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
          ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
          Some(ClientMessage::Handshake { protocol })
            if protocol.negotiate(ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS).is_some()
        ));
        ctl_ipc::write_frame(
          &mut stream,
          &ServerMessage::HandshakeAccepted {
            protocol_version: ctl_ipc::PROTOCOL_VERSION,
          },
        )
        .await
        .unwrap();
        let Some(ClientMessage::EnsureMaster { target }) =
          ctl_ipc::read_frame(&mut stream).await.unwrap()
        else {
          panic!("expected one managed master request");
        };
        ctl_ipc::write_frame(&mut stream, &ServerMessage::MasterReady { control_path })
          .await
          .unwrap();
        target
      })
      .await
      .unwrap()
    })
  }

  async fn command(
    &self,
    args: &[&str],
    executable: &std::path::Path,
    manager: Option<&std::path::Path>,
  ) -> std::process::Output {
    let mut command = tokio::process::Command::new(executable);
    command.args(args).current_dir(&self.0).kill_on_drop(true);
    command.env(
      "PATH",
      format!("{}:{}", self.0.display(), std::env::var("PATH").unwrap()),
    );
    command.env("CTL_TEST_DIR", &self.0);
    if let Some(manager) = manager {
      command.env("CTLUP_BIN", manager);
    } else {
      command.env_remove("CTLUP_BIN");
    }
    command.env_remove("CTLUP_BUNDLES_DIR");
    command.env("HOME", self.0.join("home"));
    command.env("CTL_HOSTS_PATH", self.0.join("hosts.json"));
    command.env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"));
    command.env("CTLD_BIN", self.0.join("missing-ctld"));
    // Every fixture is offline. Only explicit test data may supply a bundle.
    command.env("HTTPS_PROXY", "http://127.0.0.1:9");
    command.env("https_proxy", "http://127.0.0.1:9");
    command.env("NO_PROXY", "");
    command.env("no_proxy", "");
    command.env("CTL_REMOTE_BUNDLES_DIR", self.0.join("bundles"));
    command.env_remove("CTL_SCP_SSH_TRANSPORT");
    command.env_remove("CTLD_ASKPASS");
    tokio::time::timeout(Duration::from_secs(20), command.output())
      .await
      .unwrap_or_else(|_| {
        let channels = fs::read_to_string(self.0.join("count")).unwrap_or_else(|_| "0".into());
        panic!(
          "ctlup did not exit after {} recorded SSH channels",
          channels.trim()
        )
      })
      .unwrap()
  }

  fn arguments(&self, index: usize) -> Vec<String> {
    fs::read_to_string(self.0.join(format!("ssh-{index}.args")))
      .unwrap()
      .lines()
      .map(str::to_owned)
      .collect()
  }

  fn assert_managed_channels(&self, count: usize) {
    assert_eq!(
      fs::read_to_string(self.0.join("count")).unwrap().trim(),
      count.to_string()
    );
    for index in 1..=count {
      let arguments = self.arguments(index);
      let master = arguments.windows(2).find(|pair| pair[0] == "-S").unwrap();
      assert_eq!(master[1], self.0.join("master").to_str().unwrap());
      for option in [
        "ControlMaster=no",
        "ProxyCommand=false",
        "BatchMode=yes",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
      ] {
        assert!(
          arguments.iter().any(|argument| argument == option),
          "missing {option}: {arguments:?}"
        );
      }
      assert!(arguments.iter().any(|argument| argument == "10.0.0.20"));
    }
    assert!(!self.0.join("gh-called").exists());
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn assert_selection(fixture: &Fixture, expected: Option<&str>) {
  let selected = ctl_core::bundles::Store::new(&fixture.0.join("home"))
    .selected(ctl_core::bundles::Purpose::Upload, fixture_target())
    .unwrap();
  assert_eq!(
    selected
      .as_ref()
      .map(|bundle| bundle.manifest.bundle_id.as_str()),
    expected
  );
}
