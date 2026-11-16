#![cfg(unix)]

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ctl_ipc::{ClientMessage, ServerMessage};
use serde_json::json;

const EXPECTED_ID: &str = "fa5f874a-bd3c-4a41-9d8f-d584bd9c9a14";

#[path = "remote_repair/broker.rs"]
mod broker;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    // Keep Unix socket paths below the macOS path-length limit.
    let directory = PathBuf::from("/tmp").join(format!("ctl-repair-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Self(directory);
    fs::create_dir_all(fixture.0.join("home/.tokn/ctl")).unwrap();
    fs::write(fixture.0.join("home/.tokn/ctl/remote-id"), EXPECTED_ID).unwrap();
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
    for (command, marker) in [
      ("ssh", "count"),
      ("gh", "gh-called"),
      ("node", "builder-called"),
    ] {
      ctl_core::test_fixtures::shell_command(
        fixture.0.join(command),
        format!("touch \"$CTL_TEST_DIR/{marker}\"; exit 90\n"),
      )
      .unwrap();
    }
    fixture
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
  fn with_args(fixture: &Fixture, args: &[&str]) -> Self {
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
    command.env_remove("CTLUP_BIN");
    command.env_remove("CTLUP_BUNDLES_DIR");
    command.env("HOME", fixture.0.join("home"));
    command.env("CTL_HOSTS_PATH", fixture.0.join("hosts.json"));
    command.env("CTLD_SOCKET_PATH", fixture.0.join("ctld.sock"));
    command.env("CTLD_BIN", fixture.0.join("missing-ctld"));
    // Every fixture is offline. Only explicit test data may supply a bundle.
    command.env("HTTPS_PROXY", "http://127.0.0.1:9");
    command.env("https_proxy", "http://127.0.0.1:9");
    command.env("NO_PROXY", "");
    command.env("no_proxy", "");
    command.env("CTL_REMOTE_BUNDLES_DIR", fixture.0.join("bundles"));
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
async fn missing_or_incompatible_agent_exits_with_a_targeted_update_command() {
  let _guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (code, args, reason, hint) in [
    (
      "ctl_agent_not_found",
      &["-H", "work", "shell"][..],
      "The remote ctl-agent is missing.",
      "ctlup update --host='work'",
    ),
    (
      "ctl_agent_update_required",
      &["-H", "work", "--method", "direct", "ctmux", "list"][..],
      "The remote ctl-agent needs an update for shared SSH services.",
      "ctlup update --host='work' --method='direct'",
    ),
  ] {
    let fixture = Fixture::new();
    let original_hosts = fs::read(fixture.0.join("hosts.json")).unwrap();
    let account = fixture.0.join("home/.tokn/ctl");
    let original_id = fs::read(account.join("remote-id")).unwrap();
    let broker = broker::spawn(&fixture, code);
    let mut terminal = Terminal::with_args(&fixture, args);
    assert!(!terminal.finish().success(), "{}", terminal.text());
    let transcript = terminal.text();
    assert!(transcript.contains(reason), "{transcript}");
    assert!(
      transcript.lines().any(|line| line.trim() == hint),
      "{transcript}"
    );
    assert!(
      !transcript.contains("Install compatible remote components"),
      "{transcript}"
    );
    assert!(
      !transcript.contains("Retrying the connection"),
      "{transcript}"
    );

    let target = broker.await.unwrap();
    assert_eq!(target.destination, "10.0.0.20");
    assert_eq!(target.user.as_deref(), Some("alice"));
    assert_eq!(target.port, Some(2222));
    assert_eq!(
      fs::read_to_string(fixture.0.join("service-opens")).unwrap(),
      "1"
    );
    for marker in ["count", "builder-called", "gh-called"] {
      assert!(!fixture.0.join(marker).exists(), "unexpected {marker}");
    }
    for path in [
      "current",
      "installs",
      "bin",
      "cache",
      "components",
      "versions",
    ] {
      assert!(
        matches!(fs::symlink_metadata(account.join(path)), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "unexpected {path}"
      );
    }
    assert_eq!(fs::read(account.join("remote-id")).unwrap(), original_id);
    assert_eq!(
      fs::read(fixture.0.join("hosts.json")).unwrap(),
      original_hosts
    );
  }
}
