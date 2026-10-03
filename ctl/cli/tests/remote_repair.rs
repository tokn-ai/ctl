#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use ctl_ipc::{ClientMessage, ServerMessage};
use rustix::termios::LocalModes;
use serde_json::json;
use sha2::{Digest as _, Sha256};

const PROMPT: &str = "Install compatible remote components";
const EXPECTED_ID: &str = "fa5f874a-bd3c-4a41-9d8f-d584bd9c9a14";

fn frame(value: &serde_json::Value) -> Vec<u8> {
  let payload = serde_json::to_vec(value).unwrap();
  let mut bytes = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
  bytes.extend(payload);
  bytes
}

fn frames(mut bytes: &[u8]) -> Vec<serde_json::Value> {
  let mut messages = Vec::new();
  while !bytes.is_empty() {
    let (size, remainder) = bytes.split_at(4);
    let size = u32::from_be_bytes(size.try_into().unwrap()) as usize;
    let (payload, remainder) = remainder.split_at(size);
    messages.push(serde_json::from_slice(payload).unwrap());
    bytes = remainder;
  }
  messages
}

struct Fixture(PathBuf);

impl Fixture {
  fn new(observed_id: &str) -> Self {
    // Keep Unix socket paths below the macOS path-length limit.
    let directory = PathBuf::from("/tmp").join(format!("ctl-repair-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Self(directory);
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
    let identity = json!({
      "remote_id": observed_id,
      "agent_version": "0.1.0",
      "rmux_restart_supported": true,
    });
    let mut framed = b"ctl-ssh-v2\n".to_vec();
    framed.extend(frame(&identity));
    fs::write(fixture.0.join("identity"), framed).unwrap();
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
export CTL_TEST_CALL_INDEX="$count"
last=''
for argument do last=$argument; done
case "$last" in
  --identity)
    if [ -x "$HOME/.tokn/ctl/current/ctl-agent" ]; then
      exec "$HOME/.tokn/ctl/current/ctl-agent" --identity
    fi
    : > "$CTL_TEST_DIR/service-input-$count"
    cat "$CTL_TEST_DIR/identity"
    exec cat > "$CTL_TEST_DIR/service-input-$count"
    ;;
  *ctl-platform-v1*)
    touch "$CTL_TEST_DIR/platform"
    printf 'ctl-platform-v1\nLinux\nx86_64\n'
    ;;
  *ctl-install-progress-v1*)
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
    fixture
  }

  fn matching_bundle(&self, build: &ctl_core::component::ComponentBuildInfo) -> String {
    self.bundle(build, None)
  }

  fn compatible_bundle(&self, build: &ctl_core::component::ComponentBuildInfo) -> String {
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
    let components = json!({
      "ctl-agent": component(vec![
        protocol("ctl_identity", 3), protocol("ctl_maintenance", 2),
        protocol("ctl_remote_vpn", 1), consumed_ctld.clone(),
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
    self.bundle(build, Some(&components))
  }

  fn bundle(
    &self,
    build: &ctl_core::component::ComponentBuildInfo,
    components: Option<&serde_json::Value>,
  ) -> String {
    let revision = build.source_revision.as_deref().unwrap();
    let bundle_id = format!("{}-dev.{}", build.version, &revision[..12]);
    let target = "x86_64-unknown-linux-musl";
    let schema_version = if components.is_some() { 2 } else { 1 };
    let mut manifest = json!({
      "schema_version": schema_version,
      "app_version": build.version,
      "bundle_id": bundle_id,
      "git_revision": revision,
      "target_triple": target,
    });
    let requests = self.repaired_transport(build, &bundle_id, target);
    let payload = self.0.join("payload");
    fs::create_dir(&payload).unwrap();
    // Record all client frames before returning the final response, so
    // dropping the one-shot transport cannot race the fixture's input capture.
    fs::write(
      payload.join("ctl-agent"),
      format!(
        "#!/bin/sh\nset -eu\ncat \"$CTL_TEST_DIR/upgraded-transport\"\n\
         dd bs=1 count={requests} of=\"$CTL_TEST_DIR/service-input-$CTL_TEST_CALL_INDEX\" 2>/dev/null\n\
         cat \"$CTL_TEST_DIR/session-list\"\nexec cat > /dev/null\n"
      ),
    )
    .unwrap();
    for binary in ["ctmuxd", "ctl-taskd", "ctld"] {
      fs::write(payload.join(binary), b"#!/bin/sh\nexit 0\n").unwrap();
    }
    let mut files = serde_json::Map::new();
    for binary in ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"] {
      fs::set_permissions(payload.join(binary), fs::Permissions::from_mode(0o700)).unwrap();
      files.insert(
        binary.into(),
        json!(format!(
          "{:x}",
          Sha256::digest(fs::read(payload.join(binary)).unwrap())
        )),
      );
    }
    if let Some(components) = components {
      manifest["files"] = json!(files);
      manifest["components"] = components.clone();
    }
    fs::write(
      payload.join("manifest.json"),
      serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let archive = self.0.join(format!(
      "bundles/ctl-agent-bundle-{bundle_id}-{target}.tar.gz"
    ));
    let output = Command::new("tar")
      .env("COPYFILE_DISABLE", "1")
      .args(["--format=ustar", "-czf"])
      .arg(&archive)
      .arg("-C")
      .arg(&payload)
      .args(["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "manifest.json"])
      .output()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    let checksum = format!("{:x}", Sha256::digest(fs::read(&archive).unwrap()));
    let mut targets = serde_json::Map::new();
    for target in [
      "x86_64-unknown-linux-musl",
      "aarch64-unknown-linux-musl",
      "x86_64-apple-darwin",
      "aarch64-apple-darwin",
    ] {
      let mut entry = json!({
        "archive": format!("ctl-agent-bundle-{bundle_id}-{target}.tar.gz"),
        "sha256": checksum,
      });
      if let Some(components) = components {
        entry["components"] = components.clone();
      }
      targets.insert(target.into(), entry);
    }
    fs::write(
      self.0.join("bundles/bundle-set.json"),
      serde_json::to_vec(&json!({
        "schema_version": schema_version, "app_version": build.version,
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

  fn repaired_transport(
    &self,
    build: &ctl_core::component::ComponentBuildInfo,
    bundle_id: &str,
    target: &str,
  ) -> usize {
    let mut transport = ctl_proto::IDENTITY_PREFACE.to_vec();
    transport.extend(frame(
      &serde_json::to_value(ctl_proto::identity_protocol_offer()).unwrap(),
    ));
    transport.extend(frame(&json!({
      "remote_id": EXPECTED_ID,
      "protocols": ctl_proto::agent_protocols(),
      "agent_version": build.version,
      "build": build,
      "ctmux_restart_supported": true,
      "bundle": {
        "app_version": build.version,
        "bundle_id": bundle_id,
        "git_revision": build.source_revision,
        "target_triple": target,
      },
    })));
    transport.extend(frame(&json!({
      "type": "handshake_accepted", "protocol_version": ctmux_proto::PROTOCOL_VERSION,
      "protocols": [ctmux_proto::protocol_info()],
      "server_version": build.version, "build": build,
      "heartbeat_interval_ms": 1000, "attachment_liveness_timeout_ms": 30000,
    })));
    fs::write(self.0.join("upgraded-transport"), transport).unwrap();
    fs::write(
      self.0.join("session-list"),
      frame(&json!({"type": "session_list", "sessions": []})),
    )
    .unwrap();
    let handshake = json!({
      "type": "handshake", "protocol": ctmux_proto::protocol_offer(),
      "client_name": "ctl", "client_version": env!("CARGO_PKG_VERSION"),
    });
    let selection = json!({"protocol_version": ctl_proto::IDENTITY_PROTOCOL_VERSION});
    frame(&selection).len()
      + frame(&handshake).len()
      + frame(&json!({"type": "list_sessions"})).len()
  }

  fn script(&self, name: &str, body: &str) {
    let path = self.0.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
  }

  fn broker(&self) -> tokio::task::JoinHandle<ctl_ipc::SshTarget> {
    let listener = tokio::net::UnixListener::bind(self.0.join("ctld.sock")).unwrap();
    let master = self.0.join("master");
    tokio::spawn(async move {
      let exchange = async {
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
        ctl_ipc::write_frame(
          &mut stream,
          &ServerMessage::MasterReady {
            control_path: master,
          },
        )
        .await
        .unwrap();
        target
      };
      tokio::time::timeout(Duration::from_secs(10), exchange)
        .await
        .unwrap()
    })
  }

  fn arguments(&self, index: usize) -> Vec<String> {
    fs::read_to_string(self.0.join(format!("ssh-{index}.args")))
      .unwrap()
      .lines()
      .map(str::to_owned)
      .collect()
  }

  fn assert_managed_route(&self, count: usize) {
    assert_eq!(
      fs::read_to_string(self.0.join("count")).unwrap().trim(),
      count.to_string()
    );
    let first = self.arguments(1);
    assert_eq!(
      first,
      self.arguments(2),
      "legacy inspection must reuse the original route and fixed service command"
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

  fn assert_no_upload(&self) {
    assert!(!self.0.join("upload").exists());
    for index in [1, 2] {
      let input = fs::read(self.0.join(format!("service-input-{index}"))).unwrap();
      assert_eq!(
        input,
        [] as [u8; 0],
        "repair inspection sent a service request"
      );
    }
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

struct Terminal {
  child: Box<dyn portable_pty::Child + Send + Sync>,
  master: Option<Box<dyn portable_pty::MasterPty + Send>>,
  writer: Option<Box<dyn std::io::Write + Send>>,
  output: std::sync::mpsc::Receiver<Vec<u8>>,
  transcript: Vec<u8>,
}

impl Terminal {
  fn new(fixture: &Fixture) -> Self {
    Self::with_args(fixture, &["-H", "work", "shell"])
  }

  fn with_args(fixture: &Fixture, args: &[&str]) -> Self {
    Self::with_bundle_override(fixture, args, true)
  }

  fn with_bundle_override(fixture: &Fixture, args: &[&str], explicit: bool) -> Self {
    let pair = portable_pty::native_pty_system()
      .openpty(portable_pty::PtySize {
        rows: 40,
        cols: 120,
        ..portable_pty::PtySize::default()
      })
      .unwrap();
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_ctl"));
    command.args(args);
    command.cwd(&fixture.0);
    command.env("TERM", "xterm-256color");
    command.env(
      "PATH",
      format!("{}:{}", fixture.0.display(), std::env::var("PATH").unwrap()),
    );
    command.env("CTL_TEST_DIR", &fixture.0);
    command.env("HOME", fixture.0.join("home"));
    command.env("CTL_HOSTS_PATH", fixture.0.join("hosts.json"));
    command.env("CTLD_SOCKET_PATH", fixture.0.join("ctld.sock"));
    command.env("CTLD_BIN", fixture.0.join("missing-ctld"));
    if explicit {
      command.env("CTL_REMOTE_BUNDLES_DIR", fixture.0.join("bundles"));
    } else {
      command.env_remove("CTL_REMOTE_BUNDLES_DIR");
    }
    command.env_remove("CTL_SCP_SSH_TRANSPORT");
    command.env_remove("CTLD_ASKPASS");
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let mut buffer = [0; 4096];
      while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
          break;
        }
      }
    });
    Self {
      child,
      master: Some(pair.master),
      writer: Some(writer),
      output,
      transcript: Vec::new(),
    }
  }

  fn wait_for_prompt(&mut self) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
      if String::from_utf8_lossy(&self.transcript).contains(PROMPT) {
        return;
      }
      match self
        .output
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
      {
        Ok(bytes) => self.transcript.extend(bytes),
        Err(error) => panic!("repair prompt did not appear: {error}\n{}", self.text()),
      }
    }
  }

  fn answer(&mut self, keys: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      let termios = self.master.as_ref().unwrap().get_termios().unwrap();
      if termios.local_flags.bits() & (LocalModes::ICANON | LocalModes::ISIG).bits() == 0 {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "confirmation did not enter raw mode: {}",
        self.text()
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(1)) {
        self.transcript.extend(bytes);
      }
    }
    let writer = self.writer.as_mut().unwrap();
    writer.write_all(keys.as_bytes()).unwrap();
    writer.flush().unwrap();
  }

  fn finish(&mut self) -> portable_pty::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        break status;
      }
      assert!(
        Instant::now() < deadline,
        "CLI did not exit: {}",
        self.text()
      );
      if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
        self.transcript.extend(bytes);
      }
    };
    self.writer.take();
    self.master.take();
    loop {
      match self.output.recv_timeout(Duration::from_secs(1)) {
        Ok(bytes) => self.transcript.extend(bytes),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
          panic!("PTY did not close: {}", self.text())
        }
      }
    }
    status
  }

  fn text(&self) -> String {
    String::from_utf8_lossy(&self.transcript).into_owned()
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    if self.child.try_wait().ok().flatten().is_none() {
      let _ = self.child.kill();
      let _ = self.child.wait();
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_legacy_identity_offers_repair_and_declining_does_not_install() {
  let fixture = Fixture::new(EXPECTED_ID);
  let original = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.broker();
  let mut terminal = Terminal::new(&fixture);
  terminal.wait_for_prompt();
  terminal.answer("\r");
  assert!(!terminal.finish().success());
  let transcript = terminal.text();
  assert!(transcript.contains("ctl-ssh-v2"));
  assert!(!transcript.contains("Detecting the remote platform"));
  assert!(!fixture.0.join("platform").exists());
  fixture.assert_managed_route(2);
  fixture.assert_no_upload();
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), original);
  let target = broker.await.unwrap();
  assert_eq!(target.destination, "10.0.0.20");
  assert_eq!(target.user.as_deref(), Some("alice"));
  assert_eq!(target.port, Some(2222));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mismatched_legacy_identity_fails_before_offer_or_remote_changes() {
  let fixture = Fixture::new("0c9247af-580a-4e90-9a88-45a4f76c0ca7");
  let original = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.broker();
  let mut terminal = Terminal::new(&fixture);
  assert!(!terminal.finish().success());
  let transcript = terminal.text();
  assert!(
    transcript.contains("different remote environment"),
    "{transcript}"
  );
  assert!(!transcript.contains(PROMPT));
  assert!(!fixture.0.join("platform").exists());
  fixture.assert_managed_route(2);
  fixture.assert_no_upload();
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), original);
  broker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_repair_detects_platform_but_rejects_unverified_local_bundles_before_upload() {
  let fixture = Fixture::new(EXPECTED_ID);
  let original = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.broker();
  let mut terminal = Terminal::new(&fixture);
  terminal.wait_for_prompt();
  terminal.answer("y\r");
  assert!(!terminal.finish().success());
  let transcript = terminal.text();
  assert!(
    transcript.contains("Detecting the remote platform"),
    "{transcript}"
  );
  assert!(
    transcript.contains("Preparing compatible components for Linux x86_64"),
    "{transcript}"
  );
  // The explicit local manifest is validated even when this CLI has a dirty
  // build identity, and invalid selected content cannot upload or download.
  assert!(
    transcript.contains("invalid remote bundle-set JSON"),
    "{transcript}"
  );
  assert!(fixture.0.join("platform").exists());
  fixture.assert_managed_route(3);
  fixture.assert_no_upload();
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), original);
  broker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_repair_activates_verified_components_and_retries_once() {
  verify_successful_repair(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_repair_uses_the_managed_home_cache_and_retries_without_downloads() {
  verify_successful_repair(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compatible_older_cached_components_repair_a_development_cli_and_preserve_identity() {
  // Unlike legacy exact-source fixtures, this test must also exercise a CLI
  // compiled from a dirty checkout or without a recorded source revision.
  let build = ctl_core::component::ComponentBuildInfo {
    version: "0.0.9".into(),
    source_revision: Some("a".repeat(40)),
    source_fingerprint: "a".repeat(64),
    dirty: false,
  };
  assert_ne!(build.version, env!("CARGO_PKG_VERSION"));
  let fixture = Fixture::new(EXPECTED_ID);
  let bundle_id = fixture.compatible_bundle(&build);
  let target = "x86_64-unknown-linux-musl";
  let bundle =
    ctl_client::remote_bundle::read_verified_bundle(&[fixture.0.join("bundles")], target, &build)
      .unwrap()
      .unwrap();
  let cache = fixture.0.join("home/.tokn/ctl/agent-bundles");
  ctl_client::remote_bundle::BundleCacheEntry::new(&cache, target, &build)
    .unwrap()
    .store(&bundle)
    .unwrap();
  let original = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.broker();
  let mut terminal =
    Terminal::with_bundle_override(&fixture, &["-H", "work", "ctmux", "list"], false);
  terminal.wait_for_prompt();
  terminal.answer("y\r");
  assert_successful_repair(&fixture, &bundle_id, &original, &mut terminal, true);
  assert!(
    terminal
      .text()
      .contains("Using cached remote components 0.0.9 (aaaaaaaaaaaa)"),
    "{}",
    terminal.text(),
  );
  assert!(
    cache
      .join("a".repeat(40))
      .join(target)
      .join("bundle-set.json")
      .is_file()
  );
  broker.await.unwrap();
}

async fn verify_successful_repair(cached: bool) {
  let build = ctl_core::component::build_info();
  if build.dirty || build.source_revision.is_none() {
    eprintln!("the legacy exact-source repair fixture requires a clean source build");
    return;
  }
  let fixture = Fixture::new(EXPECTED_ID);
  let bundle_id = fixture.matching_bundle(&build);
  if cached {
    let target = "x86_64-unknown-linux-musl";
    let bundle =
      ctl_client::remote_bundle::read_verified_bundle(&[fixture.0.join("bundles")], target, &build)
        .unwrap()
        .unwrap();
    ctl_client::remote_bundle::BundleCacheEntry::new(
      &fixture.0.join("home/.tokn/ctl/agent-bundles"),
      target,
      &build,
    )
    .unwrap()
    .store(&bundle)
    .unwrap();
  }
  let original = fs::read(fixture.0.join("hosts.json")).unwrap();
  let broker = fixture.broker();
  let mut terminal =
    Terminal::with_bundle_override(&fixture, &["-H", "work", "ctmux", "list"], !cached);
  terminal.wait_for_prompt();
  terminal.answer("y\r");
  assert_successful_repair(&fixture, &bundle_id, &original, &mut terminal, cached);
  broker.await.unwrap();
}

fn assert_successful_repair(
  fixture: &Fixture,
  bundle_id: &str,
  original: &[u8],
  terminal: &mut Terminal,
  cached: bool,
) {
  assert!(terminal.finish().success(), "{}", terminal.text());
  let transcript = terminal.text();
  if cached {
    assert!(
      transcript.contains("Using cached remote components"),
      "{transcript}"
    );
  }
  assert!(
    transcript.contains("Remote components installed. Retrying the connection."),
    "{transcript}"
  );
  fixture.assert_managed_route(5);
  assert_eq!(fixture.arguments(1), fixture.arguments(5));
  assert!(fixture.0.join("platform").exists());
  assert!(fixture.0.join("upload").exists());
  let base = fixture.0.join("home/.tokn/ctl");
  assert_eq!(
    fs::read_link(base.join("current")).unwrap(),
    PathBuf::from(format!("versions/{bundle_id}"))
  );
  assert_eq!(
    fs::read_to_string(base.join("remote-id")).unwrap(),
    EXPECTED_ID
  );
  for file in ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld", "manifest.json"] {
    assert_eq!(
      fs::read(base.join("current").join(file)).unwrap(),
      fs::read(fixture.0.join("payload").join(file)).unwrap()
    );
  }
  for binary in ["ctl-agent", "ctmuxd", "ctl-taskd", "ctld"] {
    assert_eq!(
      fs::metadata(base.join("current").join(binary))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o700
    );
  }
  for index in [1, 2] {
    assert_eq!(
      fs::read(fixture.0.join(format!("service-input-{index}"))).unwrap(),
      [] as [u8; 0]
    );
  }
  let requests = frames(&fs::read(fixture.0.join("service-input-5")).unwrap());
  assert_eq!(
    requests,
    [
      json!({"protocol_version": ctl_proto::IDENTITY_PROTOCOL_VERSION}),
      json!({"type": "handshake", "protocol": ctmux_proto::protocol_offer(), "client_name": "ctl", "client_version": env!("CARGO_PKG_VERSION")}),
      json!({"type": "list_sessions"}),
    ]
  );
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), original);
}
