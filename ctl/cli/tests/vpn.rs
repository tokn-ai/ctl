#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ctl_ipc::{ClientMessage, ServerMessage, VpnSnapshot, VpnState, VpnStatus};
use rustix::termios::LocalModes;
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::time::timeout;
use unicode_width::UnicodeWidthStr as _;

const DISCONNECTED_TABLE: &str = "VPN ID  PROVIDER  STATE         SERVER  USERNAME  SOCKS5 ENDPOINT\n-       -         disconnected  -       -         -\n";

struct Fixture {
  directory: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);
    let directory = PathBuf::from("/tmp").join(format!(
      "ctl-vpn-{}-{}-{}",
      std::process::id(),
      NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    Self {
      directory: directory.canonicalize().unwrap(),
    }
  }

  fn socket(&self) -> PathBuf {
    self.directory.join("ctld.sock")
  }

  fn profiles_path(&self) -> PathBuf {
    self.directory.join("vpns.json")
  }

  fn write_profiles(&self, schema_version: u32, connections: &[serde_json::Value]) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
      "schema_version": schema_version,
      "connections": connections,
    }))
    .unwrap();
    self.write_profile_bytes(&bytes);
    bytes
  }

  fn write_profile_bytes(&self, bytes: &[u8]) {
    std::fs::write(self.profiles_path(), bytes).unwrap();
    std::fs::set_permissions(self.profiles_path(), std::fs::Permissions::from_mode(0o600)).unwrap();
  }

  fn command(&self, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(args)
      .current_dir(&self.directory)
      .env("CTLD_SOCKET_PATH", self.socket())
      .env("CTLD_BIN", self.directory.join("missing-ctld"))
      .env("CTL_VPNS_PATH", self.profiles_path())
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_VPN_SOCKET_PATH")
      .stdin(Stdio::null())
      .kill_on_drop(true);
    command
  }

  async fn output(&self, args: &[&str]) -> Output {
    timeout(Duration::from_secs(5), self.command(args).output())
      .await
      .unwrap()
      .unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

struct Terminal {
  child: Box<dyn portable_pty::Child + Send + Sync>,
  master: Option<Box<dyn portable_pty::MasterPty + Send>>,
  writer: Option<Box<dyn std::io::Write + Send>>,
  output: std::sync::mpsc::Receiver<Vec<u8>>,
  transcript: Vec<u8>,
  next_prompt: usize,
}

impl Terminal {
  fn new(fixture: &Fixture, args: &[&str]) -> Self {
    let pair = portable_pty::native_pty_system()
      .openpty(portable_pty::PtySize {
        rows: 40,
        cols: 120,
        ..portable_pty::PtySize::default()
      })
      .unwrap();
    let mut command = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_ctl"));
    command.args(args);
    command.cwd(&fixture.directory);
    command.env("TERM", "xterm-256color");
    command.env("CTLD_SOCKET_PATH", fixture.socket());
    command.env("CTLD_BIN", fixture.directory.join("missing-ctld"));
    command.env("CTL_VPNS_PATH", fixture.profiles_path());
    command.env_remove("CTLD_ASKPASS");
    command.env_remove("CTLD_VPN_SOCKET_PATH");
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
      next_prompt: 0,
    }
  }

  fn wait_for(&mut self, prompt: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      if String::from_utf8_lossy(&self.transcript[self.next_prompt..]).contains(prompt) {
        self.next_prompt = self.transcript.len();
        return;
      }
      let remaining = deadline.saturating_duration_since(Instant::now());
      match self.output.recv_timeout(remaining) {
        Ok(bytes) => self.transcript.extend(bytes),
        Err(error) => panic!(
          "did not receive prompt {prompt:?}: {error}\n{}",
          String::from_utf8_lossy(&self.transcript)
        ),
      }
    }
  }

  fn send(&mut self, keys: &str) {
    // Prompts are rendered before console enters raw mode. Wait until control
    // keys will reach the questionnaire instead of being interpreted as signals.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
      let termios = self.master.as_ref().unwrap().get_termios().unwrap();
      if termios.local_flags.bits() & (LocalModes::ICANON | LocalModes::ISIG).bits() == 0 {
        break;
      }
      assert!(
        Instant::now() < deadline,
        "terminal did not enter raw input mode before {keys:?}: {}",
        String::from_utf8_lossy(&self.transcript)
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
      if let Some(status) = self.child.try_wait().unwrap() {
        break status;
      }
      assert!(
        Instant::now() < deadline,
        "terminal command did not exit: {}",
        String::from_utf8_lossy(&self.transcript)
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
          panic!("terminal output did not close after process exit");
        }
      }
    }
    status
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

async fn reply(listener: &UnixListener, response: ServerMessage) -> ClientMessage {
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
  let request = ctl_ipc::read_frame(&mut stream).await.unwrap().unwrap();
  ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
  request
}

async fn exchange(
  fixture: &Fixture,
  listener: &UnixListener,
  args: &[&str],
  response: ServerMessage,
) -> (Output, ClientMessage) {
  let mut command = fixture.command(args);
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), reply(listener, response))
  })
  .await
  .unwrap();
  (output.unwrap(), request)
}

fn assert_json_status(output: &Output, expected: &VpnStatus) {
  assert!(output.status.success(), "{output:?}");
  assert_eq!(
    serde_json::from_slice::<VpnStatus>(&output.stdout).unwrap(),
    *expected
  );
  assert!(output.stderr.is_empty(), "{output:?}");
}

fn assert_json_snapshot(output: &Output, expected: &VpnSnapshot) {
  assert!(output.status.success(), "{output:?}");
  assert_eq!(
    serde_json::from_slice::<VpnSnapshot>(&output.stdout).unwrap(),
    *expected
  );
  assert!(output.stderr.is_empty(), "{output:?}");
}

fn response(status: VpnStatus) -> ServerMessage {
  ServerMessage::VpnStatus {
    snapshot: Some(VpnSnapshot {
      connections: if status.state == VpnState::Stopped {
        Vec::new()
      } else {
        vec![status.clone()]
      },
      supports_multiple: true,
      ..VpnSnapshot::default()
    }),
    status: Box::new(status),
  }
}

fn assert_text_status(output: &Output, expected: &str) {
  assert!(output.status.success(), "{output:?}");
  assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
  assert!(output.stderr.is_empty(), "{output:?}");
}

fn has_use_column(table: &str) -> bool {
  table
    .lines()
    .next()
    .unwrap_or_default()
    .split_whitespace()
    .any(|column| column == "USE")
}

fn connected() -> VpnStatus {
  VpnStatus {
    vpn_id: Some("test-vpn".into()),
    endpoint: Some("socks5h://127.0.0.1:43210".into()),
    vpn_url: Some("https://vpn.example.com".into()),
    username: Some("test-user".into()),
    container_name: Some("ctld-openconnect-test".into()),
    connection_id: Some("test-connection".into()),
    state: VpnState::Connected,
    running: true,
    ..VpnStatus::default()
  }
}

#[tokio::test]
async fn start_list_and_stop_use_daemon_ipc_and_print_json() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut profile = saved_openconnect();
  profile["connection_id"] = "test-connection".into();
  let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
  fixture.write_profiles(2, &[profile]);
  let ready = connected();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "start", "Work VPN", "--json"],
    response(ready.clone()),
  )
  .await;
  assert_json_status(&output, &ready);
  assert!(matches!(
    request,
    ClientMessage::StartVpnConnection { connection } if connection == expected
  ));

  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "list", "--json"],
    response(ready.clone()),
  )
  .await;
  assert_json_snapshot(
    &output,
    &VpnSnapshot {
      connections: vec![ready],
      supports_multiple: true,
      ..VpnSnapshot::default()
    },
  );
  assert!(matches!(request, ClientMessage::VpnStatus));

  let stopped = VpnStatus::default();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "stop", "--json"],
    response(stopped.clone()),
  )
  .await;
  assert_json_status(&output, &stopped);
  assert!(matches!(request, ClientMessage::StopVpn));
}

#[tokio::test]
async fn start_list_and_stop_print_a_human_readable_table_by_default() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut profile = saved_openconnect();
  profile["connection_id"] = "test-connection".into();
  fixture.write_profiles(2, &[profile]);
  for action in ["start", "list"] {
    let args = if action == "start" {
      vec!["vpn", action, "Work VPN"]
    } else {
      vec!["vpn", action]
    };
    let (output, request) = exchange(&fixture, &listener, &args, response(connected())).await;
    if action == "list" {
      assert!(matches!(request, ClientMessage::VpnStatus));
      assert!(output.status.success(), "{output:?}");
      assert!(output.stderr.is_empty(), "{output:?}");
      let table = String::from_utf8(output.stdout).unwrap();
      assert!(table.starts_with("NAME"));
      assert!(!has_use_column(&table));
      for value in [
        "OpenConnect",
        "connected",
        "https://vpn.example.com",
        "test-user",
        "socks5h://127.0.0.1:43210",
        "test-vpn",
      ] {
        assert!(table.contains(value), "{table}");
      }
    } else {
      assert_text_status(
        &output,
        "VPN ID    PROVIDER     STATE      SERVER                   USERNAME   SOCKS5 ENDPOINT\ntest-vpn  OpenConnect  connected  https://vpn.example.com  test-user  socks5h://127.0.0.1:43210\n",
      );
    }
  }
  let (output, _) = exchange(
    &fixture,
    &listener,
    &["vpn", "stop"],
    response(VpnStatus::default()),
  )
  .await;
  assert_text_status(&output, DISCONNECTED_TABLE);
}

#[tokio::test]
async fn tailscale_start_reports_pending_login_and_sends_provider_settings() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let profile = saved_tailscale();
  let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
  fixture.write_profiles(2, &[profile]);
  let pending = VpnStatus {
    provider: ctl_ipc::VpnProvider::Tailscale,
    vpn_id: Some("tailnet-id".into()),
    connection_id: Some("tailnet-id".into()),
    state: VpnState::Starting,
    auth_url: Some("https://login.tailscale.com/a/123abc".into()),
    ..VpnStatus::default()
  };
  let mut command = fixture.command(&["vpn", "start", "Team VPN"]);
  let server = async {
    assert!(matches!(
      reply(&listener, response(VpnStatus::default())).await,
      ClientMessage::VpnStatus
    ));
    reply(&listener, response(pending)).await
  };
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), server)
  })
  .await
  .unwrap();
  let output = output.unwrap();
  assert!(output.status.success(), "{output:?}");
  let text = String::from_utf8(output.stdout).unwrap();
  assert!(text.contains("Tailscale"));
  assert!(text.contains("sign-in required"));
  assert!(text.contains("Sign in for tailnet-id: https://login.tailscale.com/a/123abc"));
  assert!(
    matches!(request, ClientMessage::StartVpnConnection { connection } if connection == expected)
  );
}

#[tokio::test]
async fn start_only_reports_sign_in_for_valid_tailscale_authentication_urls() {
  for (provider, auth_url) in [
    (
      ctl_ipc::VpnProvider::Openconnect,
      "https://login.tailscale.com/a/123abc",
    ),
    (
      ctl_ipc::VpnProvider::Tailscale,
      "https://untrusted.example.test/a/123abc",
    ),
  ] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let profile = if provider == ctl_ipc::VpnProvider::Tailscale {
      saved_tailscale()
    } else {
      saved_openconnect()
    };
    let selector = profile["name"].as_str().unwrap();
    fixture.write_profiles(2, std::slice::from_ref(&profile));
    let mut command = fixture.command(&["vpn", "start", selector]);
    let server = async {
      if provider == ctl_ipc::VpnProvider::Tailscale {
        assert!(matches!(
          reply(&listener, response(VpnStatus::default())).await,
          ClientMessage::VpnStatus
        ));
      }
      reply(
        &listener,
        response(VpnStatus {
          provider,
          vpn_id: Some("pending-vpn".into()),
          state: VpnState::Starting,
          auth_url: Some(auth_url.into()),
          ..VpnStatus::default()
        }),
      )
      .await
    };
    let (output, request) = timeout(Duration::from_secs(5), async {
      tokio::join!(command.output(), server)
    })
    .await
    .unwrap();
    let output = output.unwrap();
    assert!(matches!(request, ClientMessage::StartVpnConnection { .. }));
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("starting"));
    assert!(!text.contains("sign-in required"));
    assert!(!text.contains("Sign in"));
    assert!(!text.contains(auth_url));
  }
}

#[tokio::test]
async fn tailscale_list_explains_required_device_approval() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let pending = VpnStatus {
    provider: ctl_ipc::VpnProvider::Tailscale,
    vpn_id: Some("team".into()),
    state: VpnState::Starting,
    message: Some("Approve this device in the Tailscale admin console".into()),
    ..VpnStatus::default()
  };
  let (output, _) = exchange(&fixture, &listener, &["vpn", "list"], response(pending)).await;
  assert!(output.status.success());
  let text = String::from_utf8(output.stdout).unwrap();
  assert!(text.contains("team: Approve this device in the Tailscale admin console"));
}

#[tokio::test]
async fn human_list_shows_lifecycle_and_unavailable_connection_details() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  for (state, expected) in [
    (VpnState::Starting, "starting"),
    (VpnState::Connected, "connected"),
    (VpnState::Stopping, "stopping"),
  ] {
    let (output, _) = exchange(
      &fixture,
      &listener,
      &["vpn", "list"],
      response(VpnStatus {
        state,
        ..VpnStatus::default()
      }),
    )
    .await;
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let table = String::from_utf8(output.stdout).unwrap();
    let rows: Vec<_> = table.lines().collect();
    assert_eq!(rows.len(), 2);
    assert!(rows[1].contains("OpenConnect"));
    assert!(rows[1].contains(expected));
    assert!(!rows[1].contains("socks5h://"));
  }
}

#[tokio::test]
async fn human_list_escapes_terminal_controls_and_preserves_readable_unicode() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let (output, _) = exchange(
    &fixture,
    &listener,
    &["vpn", "list"],
    response(VpnStatus {
      vpn_id: Some("test\n-vpn".into()),
      vpn_url: Some("https://vpn.example.com\u{001b}[2J".into()),
      username: Some("用户\nadmin\t\u{202e}\u{0085}".into()),
      endpoint: Some("socks5h://127.0.0.1:43210\r".into()),
      ..connected()
    }),
  )
  .await;
  assert!(output.status.success());
  assert_eq!(output.stderr, Vec::<u8>::new());
  let text = String::from_utf8(output.stdout).unwrap();
  let lines: Vec<_> = text.lines().collect();
  assert_eq!(lines.len(), 2);
  assert!(lines[1].contains("test\\n-vpn"));
  assert!(lines[1].contains("用户\\nadmin\\t\\u{202e}\\u{85}"));
  assert!(lines[1].contains("socks5h://127.0.0.1:43210\\r"));
  assert!(
    text
      .chars()
      .all(|character| character == '\n' || !character.is_control())
  );
  let heading_start = lines[0].find("SOCKS5 ENDPOINT").unwrap();
  let endpoint_start = lines[1].find("socks5h://").unwrap();
  assert_eq!(
    lines[0][..heading_start].width(),
    lines[1][..endpoint_start].width()
  );
}

#[tokio::test]
async fn list_shows_multiple_connections_and_aligns_every_column() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let first = connected();
  let second = VpnStatus {
    vpn_id: Some("another-vpn-with-a-long-id".into()),
    vpn_url: Some("https://another-gateway.example.com".into()),
    username: Some("用户".into()),
    endpoint: Some("socks5h://127.0.0.1:43211".into()),
    ..connected()
  };
  let snapshot = VpnSnapshot {
    connections: vec![first.clone(), second],
    supports_multiple: true,
    ..VpnSnapshot::default()
  };
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "list"],
    ServerMessage::VpnStatus {
      status: Box::new(first.clone()),
      snapshot: Some(snapshot.clone()),
    },
  )
  .await;
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert!(output.status.success());
  assert_eq!(output.stderr, Vec::<u8>::new());
  let text = String::from_utf8(output.stdout).unwrap();
  let lines: Vec<_> = text.lines().collect();
  assert_eq!(lines.len(), 3);
  for (line, connection) in lines[1..].iter().zip(&snapshot.connections) {
    assert!(line.contains(connection.vpn_id.as_deref().unwrap()));
    for (heading, value) in [
      ("STATE", "connected"),
      ("SERVER", connection.vpn_url.as_deref().unwrap()),
      ("USERNAME", connection.username.as_deref().unwrap()),
      ("SOCKS5 ENDPOINT", connection.endpoint.as_deref().unwrap()),
    ] {
      assert_eq!(
        lines[0][..lines[0].find(heading).unwrap()].width(),
        line[..line.find(value).unwrap()].width(),
        "column {heading} is not aligned: {text}"
      );
    }
  }
  let (output, _) = exchange(
    &fixture,
    &listener,
    &["vpn", "list", "--json"],
    ServerMessage::VpnStatus {
      status: Box::new(first),
      snapshot: Some(snapshot.clone()),
    },
  )
  .await;
  assert_json_snapshot(&output, &snapshot);
}

#[tokio::test]
async fn legacy_status_is_exposed_as_a_single_connection_snapshot() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let legacy = VpnStatus {
    vpn_id: None,
    ..connected()
  };
  let (output, _) = exchange(
    &fixture,
    &listener,
    &["vpn", "list", "--json"],
    ServerMessage::VpnStatus {
      status: Box::new(legacy.clone()),
      snapshot: None,
    },
  )
  .await;
  assert_json_snapshot(
    &output,
    &VpnSnapshot {
      connections: vec![VpnStatus {
        vpn_id: legacy.connection_id.clone(),
        ..legacy
      }],
      supports_multiple: false,
      supported_providers: vec![ctl_ipc::VpnProvider::Openconnect],
      supports_tailscale_enrollment: false,
      discovery_warnings: Vec::new(),
    },
  );
}

#[tokio::test]
async fn targeted_stop_selects_one_vpn_and_rejects_unsafe_legacy_fallback() {
  for supports_multiple in [true, false] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let connected = VpnStatus {
      vpn_id: None,
      ..connected()
    };
    let selected_id = connected.connection_id.as_deref().unwrap();
    let stopped = VpnStatus {
      vpn_id: Some(selected_id.into()),
      ..VpnStatus::default()
    };
    let mut command = fixture.command(&["vpn", "stop", selected_id, "--json"]);
    let server = async {
      let initial = if supports_multiple {
        ServerMessage::VpnStatus {
          status: Box::new(connected.clone()),
          snapshot: Some(VpnSnapshot {
            connections: vec![
              VpnStatus {
                vpn_id: Some(selected_id.into()),
                ..connected.clone()
              },
              VpnStatus {
                vpn_id: Some("other-vpn".into()),
                ..connected.clone()
              },
            ],
            supports_multiple,
            ..VpnSnapshot::default()
          }),
        }
      } else {
        ServerMessage::VpnStatus {
          status: Box::new(connected.clone()),
          snapshot: None,
        }
      };
      let probe = reply(&listener, initial).await;
      assert!(matches!(probe, ClientMessage::VpnStatus));
      if supports_multiple {
        Some(reply(&listener, response(stopped.clone())).await)
      } else {
        None
      }
    };
    let (output, request) = timeout(Duration::from_secs(5), async {
      tokio::join!(command.output(), server)
    })
    .await
    .unwrap();
    let output = output.unwrap();
    if supports_multiple {
      assert_json_status(&output, &stopped);
      assert!(
        matches!(request, Some(ClientMessage::StopVpnById { vpn_id }) if vpn_id == selected_id)
      );
    } else {
      assert!(request.is_none());
      assert!(!output.status.success());
      assert_eq!(output.stdout, Vec::<u8>::new());
      let diagnostic = String::from_utf8(output.stderr).unwrap();
      assert!(diagnostic.contains("vpn_targeted_stop_unsupported"));
      assert!(diagnostic.contains("ctl vpn stop"));
    }
  }
}

#[tokio::test]
async fn targeted_stop_does_not_stop_a_different_legacy_connection() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "stop", "different-vpn", "--json"],
    ServerMessage::VpnStatus {
      status: Box::new(VpnStatus {
        vpn_id: None,
        ..connected()
      }),
      snapshot: None,
    },
  )
  .await;
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert!(!output.status.success());
  assert_eq!(output.stdout, Vec::<u8>::new());
  assert!(
    String::from_utf8(output.stderr)
      .unwrap()
      .contains("vpn_not_found")
  );
}

#[tokio::test]
async fn stop_accepts_saved_names_ids_and_runtime_ids_despite_an_unreadable_catalog() {
  for (selector, runtime_id, valid_catalog) in [
    ("Work VPN", "work-id", true),
    ("work-id", "work-id", true),
    ("runtime-only-id", "runtime-only-id", false),
  ] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    if valid_catalog {
      fixture.write_profiles(2, &[saved_openconnect()]);
    } else {
      fixture.write_profile_bytes(br#"{"schema_version":"saved-private-secret"}"#);
    }
    let ready = VpnStatus {
      vpn_id: Some(runtime_id.into()),
      ..connected()
    };
    let stopped = VpnStatus {
      vpn_id: Some(runtime_id.into()),
      ..VpnStatus::default()
    };
    let mut command = fixture.command(&["vpn", "stop", selector, "--json"]);
    let server = async {
      assert!(matches!(
        reply(&listener, response(ready)).await,
        ClientMessage::VpnStatus
      ));
      reply(&listener, response(stopped.clone())).await
    };
    let (output, request) = timeout(Duration::from_secs(5), async {
      tokio::join!(command.output(), server)
    })
    .await
    .unwrap();
    assert_json_status(&output.unwrap(), &stopped);
    assert!(matches!(request, ClientMessage::StopVpnById { vpn_id } if vpn_id == runtime_id));
  }
}

#[tokio::test]
async fn stop_without_an_id_reports_ambiguity_and_never_stops_all_connections() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "stop"],
    ServerMessage::Error {
      code: "vpn_failed".into(),
      message: "Multiple VPNs are active; select a VPN ID".into(),
    },
  )
  .await;
  assert!(matches!(request, ClientMessage::StopVpn));
  assert!(!output.status.success());
  assert_eq!(output.stdout, Vec::<u8>::new());
  assert!(
    String::from_utf8(output.stderr)
      .unwrap()
      .contains("select a VPN ID")
  );
}

#[tokio::test]
async fn missing_daemon_reports_unavailable_inventory_without_starting_and_remote_profile_mutations_are_rejected()
 {
  let fixture = Fixture::new();
  for action in ["list", "stop"] {
    let output = fixture.command(&["vpn", action]).output().await.unwrap();
    if action == "list" {
      assert!(output.status.success());
      let text = String::from_utf8(output.stdout).unwrap();
      assert!(text.contains("ctld is not running"));
      assert_eq!(output.stderr, Vec::<u8>::new());
    } else {
      assert_text_status(&output, DISCONNECTED_TABLE);
    }
    let output = fixture
      .command(&["vpn", action, "--json"])
      .output()
      .await
      .unwrap();
    if action == "list" {
      assert!(output.status.success());
      let snapshot: VpnSnapshot = serde_json::from_slice(&output.stdout).unwrap();
      assert_eq!(snapshot.connections, Vec::<ctl_ipc::VpnStatus>::new());
      assert_eq!(snapshot.discovery_warnings.len(), 1);
    } else {
      assert_json_status(&output, &VpnStatus::default());
    }
    assert!(!fixture.socket().exists());
  }
  for action in ["create", "remove"] {
    for json in [false, true] {
      let mut args = vec!["--host", "vpn-host", "vpn", action];
      if json {
        args.push("--json");
      }
      let output = fixture.command(&args).output().await.unwrap();
      assert!(!output.status.success());
      assert_eq!(output.stdout, Vec::<u8>::new());
      assert!(
        String::from_utf8(output.stderr)
          .unwrap()
          .contains("omit --host")
      );
    }
  }
}

#[tokio::test]
async fn saved_start_launches_ctld_when_absent() {
  let fixture = Fixture::new();
  let profile = saved_openconnect();
  let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
  fixture.write_profiles(2, &[profile]);
  let helper = fixture.directory.join("ctld");
  let marker = fixture.directory.join("started");
  let binary = ctl_ipc::lifecycle::DaemonBinaryInfo::current();
  let metadata = serde_json::to_string(&serde_json::json!({
    "build": binary.build,
    "protocols": binary.protocols,
  }))
  .unwrap();
  std::fs::write(
    &helper,
    format!(
      "#!/bin/sh\ncase \"$1\" in\n  --component-info) printf '%s\\n' '{metadata}';;\n  --socket) touch \"$CTL_VPN_TEST_MARKER\";;\n  *) exit 2;;\nesac\n"
    ),
  )
  .unwrap();
  std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
  let mut command = fixture.command(&["vpn", "start", "work-id"]);
  command
    .env("CTLD_BIN", helper)
    .env("CTL_VPN_TEST_MARKER", &marker);
  let stopped = VpnStatus::default();
  let server = async {
    while !marker.exists() {
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    reply(&listener, response(stopped.clone())).await
  };
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), server)
  })
  .await
  .unwrap();
  assert_text_status(&output.unwrap(), DISCONNECTED_TABLE);
  assert!(matches!(
    request,
    ClientMessage::StartVpnConnection { connection } if connection == expected
  ));
}

#[tokio::test]
async fn daemon_errors_are_reported_without_success_output() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  fixture.write_profiles(2, &[saved_openconnect()]);
  for args in [
    vec!["vpn", "start", "work-id"],
    vec!["vpn", "start", "work-id", "--json"],
  ] {
    let (output, request) = exchange(
      &fixture,
      &listener,
      &args,
      ServerMessage::Error {
        code: "vpn_start_failed".into(),
        message: "synthetic startup failure".into(),
      },
    )
    .await;
    assert!(matches!(request, ClientMessage::StartVpnConnection { .. }));
    assert!(!output.status.success());
    assert_eq!(output.stdout, Vec::<u8>::new());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("vpn_start_failed"));
    assert!(stderr.contains("synthetic startup failure"));
  }
}

#[tokio::test]
async fn shared_container_list_distinguishes_local_interest_and_keeps_endpoint() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let snapshot = VpnSnapshot {
    connections: vec![
      VpnStatus {
        vpn_id: Some("local-profile".into()),
        shared_container: true,
        locally_connected: Some(true),
        ..connected()
      },
      VpnStatus {
        vpn_id: Some("shared-profile".into()),
        shared_container: true,
        locally_connected: Some(false),
        ..connected()
      },
    ],
    discovery_warnings: vec!["Some container metadata could not be read".into()],
    ..VpnSnapshot::default()
  };
  let (output, _) = exchange(
    &fixture,
    &listener,
    &["vpn", "list"],
    ServerMessage::VpnStatus {
      status: Box::new(snapshot.connections[0].clone()),
      snapshot: Some(snapshot),
    },
  )
  .await;
  assert!(output.status.success());
  let text = String::from_utf8(output.stdout).unwrap();
  let rows: Vec<_> = text.lines().collect();
  assert!(has_use_column(&text));
  assert!(rows[1].contains("owned"));
  assert!(!text.contains("this ctld"));
  assert!(rows[2].contains("shared"));
  assert!(rows[2].contains("connected"));
  assert!(rows[2].contains("socks5h://127.0.0.1:43210"));
  assert!(text.contains("Warning: Some container metadata could not be read"));
}

#[tokio::test]
async fn vpn_tables_only_show_use_when_a_connection_is_displayed_as_shared() {
  for locally_connected in [Some(true), None, Some(false)] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    fixture.write_profiles(2, &[saved_openconnect()]);
    let status = VpnStatus {
      shared_container: true,
      locally_connected,
      connection_id: Some("work-id".into()),
      ..connected()
    };
    for action in ["list", "start", "stop"] {
      let args = if action == "start" {
        vec!["vpn", action, "work-id"]
      } else {
        vec!["vpn", action]
      };
      let (output, _) = exchange(&fixture, &listener, &args, response(status.clone())).await;
      assert!(output.status.success(), "{output:?}");
      assert!(output.stderr.is_empty(), "{output:?}");
      let table = String::from_utf8(output.stdout).unwrap();
      assert_eq!(
        has_use_column(&table),
        locally_connected == Some(false),
        "{action}: {table}"
      );
      if locally_connected == Some(false) {
        assert!(table.lines().nth(1).unwrap().contains("shared"));
      }
      assert!(!table.contains("this ctld"));
      assert!(table.contains("socks5h://127.0.0.1:43210"));
    }
  }
}

#[tokio::test]
async fn default_vpn_client_honors_the_vpn_socket_override() {
  let fixture = Fixture::new();
  let selected = fixture.directory.join("vpn-override.sock");
  let listener = UnixListener::bind(&selected).unwrap();
  let unused = UnixListener::bind(fixture.socket()).unwrap();
  let mut command = fixture.command(&["vpn", "list", "--json"]);
  command
    .env("CTLD_VPN_SOCKET_PATH", &selected)
    .env("CTMUX_DEV_DAEMON_SUPERVISOR", "1");
  let (output, request) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), reply(&listener, response(connected())))
  })
  .await
  .unwrap();
  assert!(output.unwrap().status.success());
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert!(
    timeout(Duration::from_millis(30), unused.accept())
      .await
      .is_err()
  );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn saved_host_reuses_connected_vpn_without_resolving_a_helper() {
  use std::os::unix::fs::PermissionsExt as _;

  let fixture = Fixture::new();
  let hosts = fixture.directory.join("hosts.json");
  std::fs::write(
    &hosts,
    serde_json::to_vec(&serde_json::json!({
      "revision": "test",
      "document": {
        "schema_version": 1,
        "ssh_gateways": [],
        "hosts": [{
          "host_id": "work",
          "name": "work",
          "preferred_method_id": "vpn",
          "connection_methods": [{
            "method_id": "vpn",
            "name": "VPN",
            "target": {
              "kind": "ssh",
              "destination": "host.example.invalid",
              "vpn_connection_id": "test-vpn"
            }
          }]
        }]
      }
    }))
    .unwrap(),
  )
  .unwrap();
  // This cache would fail executable discovery. Reusing the existing owner
  // should never inspect it, even if an old supervisor flag was inherited.
  let mut component = fixture.directory.clone();
  for name in [".tokn", "ctl", "components", "ctld", "versions"] {
    component.push(name);
    std::fs::create_dir(&component).unwrap();
    std::fs::set_permissions(&component, std::fs::Permissions::from_mode(0o700)).unwrap();
  }
  std::fs::write(
    component.parent().unwrap().join("current"),
    "invalid selection",
  )
  .unwrap();
  let ssh = fixture.directory.join("ssh");
  std::fs::write(&ssh, "#!/bin/sh\nexit 0\n").unwrap();
  std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut command = fixture.command(&["-H", "work", "exec", "--", "true"]);
  command
    .env("HOME", &fixture.directory)
    .env("PATH", &fixture.directory)
    .env("CTL_HOSTS_PATH", &hosts)
    .env("CTLD_VPN_SOCKET_PATH", fixture.socket())
    .env("CTMUX_DEV_DAEMON_SUPERVISOR", "1");
  let server = async {
    assert!(matches!(
      reply(&listener, response(connected())).await,
      ClientMessage::VpnStatus
    ));
    let request = reply(
      &listener,
      ServerMessage::MasterReady {
        control_path: fixture.directory.join("existing-master"),
      },
    )
    .await;
    assert!(matches!(request, ClientMessage::EnsureMaster { .. }));
  };
  let (output, ()) = timeout(Duration::from_secs(5), async {
    tokio::join!(command.output(), server)
  })
  .await
  .unwrap();
  let output = output.unwrap();
  assert!(output.status.success(), "{output:?}");
  assert_eq!(output.stdout, Vec::<u8>::new());
  assert_eq!(output.stderr, Vec::<u8>::new());
}

#[tokio::test]
async fn released_shared_container_with_unavailable_status_retains_metadata_without_claiming_connected()
 {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let retained = VpnStatus {
    shared_container: true,
    locally_connected: Some(false),
    status_unavailable: true,
    ..connected()
  };
  let (output, _) = exchange(&fixture, &listener, &["vpn", "stop"], response(retained)).await;
  assert!(output.status.success());
  let text = String::from_utf8(output.stdout).unwrap();
  assert!(text.contains("unavailable"));
  assert!(text.contains("shared"));
  assert!(text.contains("https://vpn.example.com"));
  assert!(text.contains("socks5h://127.0.0.1:43210"));
  assert!(!text.contains("connected"));
}

fn saved_openconnect() -> serde_json::Value {
  serde_json::json!({
    "connection_id": "work-id",
    "name": "Work VPN",
    "provider": "openconnect",
    "url": "https://vpn.saved.example.test/group?token=saved-private-token",
    "username": "saved-test-user",
    "password": "saved-private-secret",
    "auth_method": "certificate-group",
    "target_ip": "10.1.2.3",
  })
}

fn saved_tailscale() -> serde_json::Value {
  serde_json::json!({
    "connection_id": "tailnet-id",
    "name": "Team VPN",
    "provider": "tailscale",
    "hostname": "ctl-test",
    "accept_routes": true,
  })
}

fn assert_no_saved_credentials(output: &Output) {
  for value in [
    "vpn.saved.example.test",
    "saved-test-user",
    "saved-private-secret",
    "certificate-group",
  ] {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
  }
}

fn assert_no_profile_secrets(output: &Output) {
  for value in [
    "saved-private-secret",
    "saved-private-token",
    "certificate-group",
    "/group",
    "\"password\"",
    "\"auth_method\"",
  ] {
    assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
  }
}

fn inventory(output: &Output) -> serde_json::Value {
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  serde_json::from_slice(&output.stdout).unwrap()
}

fn entry_summaries(inventory: &serde_json::Value) -> serde_json::Value {
  serde_json::Value::Array(
    inventory["entries"]
      .as_array()
      .unwrap()
      .iter()
      .map(|entry| {
        serde_json::json!({
          "connection_id": entry["connection_id"],
          "name": entry["name"],
          "provider": entry["provider"],
          "saved": entry["saved"],
          "state": entry["state"],
        })
      })
      .collect(),
  )
}

async fn assert_no_daemon_contact(listener: &UnixListener) {
  assert!(
    timeout(Duration::from_millis(30), listener.accept())
      .await
      .is_err()
  );
}

#[tokio::test]
async fn list_reads_saved_profiles_and_passively_checks_runtime_without_exposing_secrets() {
  for schema_version in [1, 2] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let mut openconnect = saved_openconnect();
    let mut profiles = vec![openconnect.clone()];
    let mut expected = vec![serde_json::json!({
      "connection_id": "work-id",
      "name": "Work VPN",
      "provider": "openconnect",
      "saved": true,
      "state": "disconnected",
    })];
    if schema_version == 1 {
      openconnect.as_object_mut().unwrap().remove("provider");
      profiles[0] = openconnect;
    } else {
      profiles.push(saved_tailscale());
      expected.push(serde_json::json!({
        "connection_id": "tailnet-id",
        "name": "Team VPN",
        "provider": "tailscale",
        "saved": true,
        "state": "disconnected",
      }));
    }
    let original = fixture.write_profiles(schema_version, &profiles);
    let (output, request) = exchange(
      &fixture,
      &listener,
      &["vpn", "list", "--json"],
      response(VpnStatus::default()),
    )
    .await;
    assert!(matches!(request, ClientMessage::VpnStatus));
    let value = inventory(&output);
    assert_eq!(entry_summaries(&value), serde_json::json!(expected));
    assert_eq!(value["connections"], serde_json::json!([]));
    assert_eq!(value["supports_multiple"], true);
    assert_eq!(
      value["entries"][0]["server"],
      "https://vpn.saved.example.test"
    );
    assert_eq!(value["entries"][0]["username"], "saved-test-user");
    assert_no_profile_secrets(&output);

    let (output, request) = exchange(
      &fixture,
      &listener,
      &["vpn", "list"],
      response(VpnStatus::default()),
    )
    .await;
    assert!(matches!(request, ClientMessage::VpnStatus));
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_no_profile_secrets(&output);
    let table = String::from_utf8(output.stdout).unwrap();
    assert!(!has_use_column(&table));
    let lines: Vec<_> = table.lines().collect();
    assert_eq!(lines.len(), profiles.len() + 1);
    assert!(lines[0].contains("VPN ID"));
    assert!(lines[0].contains("NAME"));
    assert!(lines[0].contains("PROVIDER"));
    assert!(lines[1].contains("work-id"));
    assert!(lines[1].contains("Work VPN"));
    assert!(lines[1].contains("OpenConnect"));
    if schema_version == 2 {
      assert!(lines[2].contains("tailnet-id"));
      assert!(lines[2].contains("Team VPN"));
      assert!(lines[2].contains("Tailscale"));
    }
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert_no_daemon_contact(&listener).await;
  }
}

#[tokio::test]
async fn list_without_a_saved_catalog_is_empty_and_does_not_start_ctld() {
  let fixture = Fixture::new();
  let output = fixture.output(&["vpn", "list", "--json"]).await;
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  let value = inventory(&output);
  assert_eq!(value["entries"], serde_json::json!([]));
  assert_eq!(value["connections"], serde_json::json!([]));
  assert_eq!(value["discovery_warnings"].as_array().unwrap().len(), 1);
  assert!(!fixture.socket().exists());
  assert!(!fixture.profiles_path().exists());

  let output = fixture
    .output(&["vpn", "start", "missing-profile", "--json"])
    .await;
  assert!(!output.status.success(), "{output:?}");
  assert!(output.stdout.is_empty(), "{output:?}");
  assert!(String::from_utf8_lossy(&output.stderr).contains("ctl vpn list"));
  assert!(!fixture.socket().exists());
  assert!(!fixture.profiles_path().exists());
}

#[tokio::test]
async fn list_uses_the_desktop_catalog_path_by_default() {
  let fixture = Fixture::new();
  let bytes = fixture.write_profiles(2, &[saved_openconnect()]);
  let directory = fixture.directory.join(".tokn/ctl");
  std::fs::create_dir_all(&directory).unwrap();
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  let path = directory.join("vpns.json");
  std::fs::rename(fixture.profiles_path(), &path).unwrap();
  let mut command = fixture.command(&["vpn", "list", "--json"]);
  command
    .env("HOME", &fixture.directory)
    .env_remove("CTL_VPNS_PATH");
  let output = timeout(Duration::from_secs(5), command.output())
    .await
    .unwrap()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  let value = inventory(&output);
  assert_eq!(
    entry_summaries(&value),
    serde_json::json!([{
      "connection_id": "work-id",
      "name": "Work VPN",
      "provider": "openconnect",
      "saved": true,
      "state": "unavailable",
    }])
  );
  assert_no_profile_secrets(&output);
  assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[tokio::test]
async fn list_merges_saved_connected_disconnected_and_runtime_only_connections() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
  let selected = VpnStatus {
    connection_id: Some("work-id".into()),
    vpn_url: Some("https://vpn.example.com/runtime-group?token=runtime-private-token".into()),
    ..connected()
  };
  let unsaved = VpnStatus {
    provider: ctl_ipc::VpnProvider::Tailscale,
    vpn_id: Some("unsaved-vpn".into()),
    connection_id: Some("unsaved-profile".into()),
    endpoint: Some("socks5h://127.0.0.1:43211".into()),
    tailnet: Some("team.example.test".into()),
    vpn_url: None,
    username: None,
    ..connected()
  };
  let snapshot = VpnSnapshot {
    connections: vec![selected.clone(), unsaved],
    supports_multiple: true,
    ..VpnSnapshot::default()
  };
  let response = || ServerMessage::VpnStatus {
    status: Box::new(selected.clone()),
    snapshot: Some(snapshot.clone()),
  };
  let (output, request) =
    exchange(&fixture, &listener, &["vpn", "list", "--json"], response()).await;
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert_json_snapshot(&output, &snapshot);
  let value = inventory(&output);
  assert_eq!(
    entry_summaries(&value),
    serde_json::json!([
      {"connection_id": "work-id", "name": "Work VPN", "provider": "openconnect", "saved": true, "state": "connected"},
      {"connection_id": "tailnet-id", "name": "Team VPN", "provider": "tailscale", "saved": true, "state": "disconnected"},
      {"connection_id": "unsaved-profile", "name": null, "provider": "tailscale", "saved": false, "state": "connected"},
    ])
  );
  assert_eq!(value["entries"][0]["vpn_id"], "test-vpn");
  assert_eq!(value["entries"][0]["server"], "https://vpn.example.com");
  assert_eq!(value["entries"][0]["username"], "test-user");
  assert_eq!(value["entries"][0]["endpoint"], "socks5h://127.0.0.1:43210");
  assert!(value["entries"][1]["endpoint"].is_null());
  assert_eq!(value["entries"][2]["endpoint"], "socks5h://127.0.0.1:43211");
  let entries = serde_json::to_string(&value["entries"]).unwrap();
  assert!(!entries.contains("runtime-group"));
  assert!(!entries.contains("runtime-private-token"));
  assert_no_profile_secrets(&output);

  let (output, request) = exchange(&fixture, &listener, &["vpn", "list"], response()).await;
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert!(output.status.success(), "{output:?}");
  assert!(output.stderr.is_empty(), "{output:?}");
  assert_no_profile_secrets(&output);
  let table = String::from_utf8(output.stdout).unwrap();
  assert!(!has_use_column(&table));
  let rows: Vec<_> = table.lines().collect();
  assert_eq!(rows.len(), 4);
  assert!(rows[1].contains("Work VPN"));
  assert!(rows[1].contains("connected"));
  assert!(rows[1].contains("socks5h://127.0.0.1:43210"));
  assert!(rows[2].contains("Team VPN"));
  assert!(rows[2].contains("disconnected"));
  assert!(!rows[2].contains("socks5h://"));
  assert!(rows[3].contains("unsaved-vpn"));
  assert!(rows[3].contains("socks5h://127.0.0.1:43211"));
}

#[tokio::test]
async fn list_marks_unmatched_saved_profiles_unavailable_when_runtime_inventory_is_missing() {
  for daemon_running in [false, true] {
    let fixture = Fixture::new();
    fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
    let output = if daemon_running {
      let listener = UnixListener::bind(fixture.socket()).unwrap();
      let (output, request) = exchange(
        &fixture,
        &listener,
        &["vpn", "list", "--json"],
        ServerMessage::Error {
          code: "vpn_inventory_failed".into(),
          message: "Synthetic inventory read failure".into(),
        },
      )
      .await;
      assert!(matches!(request, ClientMessage::VpnStatus));
      output
    } else {
      fixture.output(&["vpn", "list", "--json"]).await
    };
    let value = inventory(&output);
    assert_eq!(value["entries"].as_array().unwrap().len(), 2);
    assert_eq!(value["connections"], serde_json::json!([]));
    assert_ne!(
      value["discovery_warnings"].as_array().unwrap().as_slice(),
      &[] as &[serde_json::Value]
    );
    for entry in value["entries"].as_array().unwrap() {
      assert_eq!(entry["saved"], true);
      assert_eq!(entry["state"], "unavailable");
      assert!(entry["endpoint"].is_null());
    }
    assert_no_profile_secrets(&output);
    if !daemon_running {
      assert!(!fixture.socket().exists());
    }
  }
}

#[tokio::test]
async fn list_preserves_verified_connections_during_partial_inventory_and_hides_unverified_endpoints()
 {
  for unavailable in [false, true] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
    let selected = VpnStatus {
      connection_id: Some("work-id".into()),
      status_unavailable: unavailable,
      ..connected()
    };
    let snapshot = VpnSnapshot {
      connections: vec![selected.clone()],
      discovery_warnings: vec!["Partial VPN container inventory".into()],
      ..VpnSnapshot::default()
    };
    let (output, request) = exchange(
      &fixture,
      &listener,
      &["vpn", "list", "--json"],
      ServerMessage::VpnStatus {
        status: Box::new(selected),
        snapshot: Some(snapshot.clone()),
      },
    )
    .await;
    assert!(matches!(request, ClientMessage::VpnStatus));
    assert_json_snapshot(&output, &snapshot);
    let value = inventory(&output);
    assert_eq!(value["entries"].as_array().unwrap().len(), 2);
    assert_eq!(
      value["entries"][0]["state"],
      if unavailable {
        "unavailable"
      } else {
        "connected"
      }
    );
    if unavailable {
      assert!(value["entries"][0]["endpoint"].is_null());
    } else {
      assert_eq!(value["entries"][0]["endpoint"], "socks5h://127.0.0.1:43210");
    }
    assert_eq!(value["entries"][1]["state"], "unavailable");
    assert!(value["entries"][1]["endpoint"].is_null());
    assert_no_profile_secrets(&output);
  }
}

#[tokio::test]
async fn legacy_local_inventory_keeps_matched_connections_and_marks_unmatched_profiles_unavailable()
{
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
  let selected = VpnStatus {
    connection_id: Some("work-id".into()),
    ..connected()
  };
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "list", "--json"],
    ServerMessage::VpnStatus {
      status: Box::new(selected.clone()),
      snapshot: None,
    },
  )
  .await;
  assert!(matches!(request, ClientMessage::VpnStatus));
  assert_json_snapshot(
    &output,
    &VpnSnapshot {
      connections: vec![selected],
      supports_multiple: false,
      supported_providers: vec![ctl_ipc::VpnProvider::Openconnect],
      supports_tailscale_enrollment: false,
      discovery_warnings: Vec::new(),
    },
  );
  let value = inventory(&output);
  assert_eq!(
    entry_summaries(&value),
    serde_json::json!([
      {"connection_id": "work-id", "name": "Work VPN", "provider": "openconnect", "saved": true, "state": "connected"},
      {"connection_id": "tailnet-id", "name": "Team VPN", "provider": "tailscale", "saved": true, "state": "unavailable"},
    ])
  );
  assert_eq!(value["entries"][0]["endpoint"], "socks5h://127.0.0.1:43210");
  assert!(value["entries"][1]["endpoint"].is_null());
  assert_no_profile_secrets(&output);
}

#[tokio::test]
async fn status_is_rejected_and_list_is_the_only_inventory_command() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  for args in [vec!["vpn", "status"], vec!["vpn", "status", "--json"]] {
    let output = fixture.output(&args).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized subcommand"));
    assert_no_daemon_contact(&listener).await;
  }
  let output = fixture.output(&["vpn", "--help"]).await;
  assert!(output.status.success(), "{output:?}");
  let help = String::from_utf8(output.stdout).unwrap();
  assert!(
    help
      .lines()
      .any(|line| line.trim_start().starts_with("list "))
  );
  assert!(
    !help
      .lines()
      .any(|line| line.trim_start().starts_with("status "))
  );
}

#[tokio::test]
async fn removed_vpn_commands_and_flags_are_rejected_without_mutation_or_ipc() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect()]);
  for args in [
    vec!["vpn", "connect", "Work VPN"],
    vec!["vpn", "start-tailscale", "--id", "team"],
    vec!["vpn", "start", "--env-file", "work.env"],
    vec!["vpn", "start", "work-id", "--env-file", "work.env"],
    vec!["vpn", "start", "work-id", "--hostname", "ctl-test"],
    vec!["vpn", "start", "work-id", "--accept-routes"],
  ] {
    let output = fixture.output(&args).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
    assert_no_saved_credentials(&output);
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert_no_daemon_contact(&listener).await;
  }
}

#[tokio::test]
async fn create_and_start_without_selector_require_a_terminal_without_mutation_or_ipc() {
  for existing_catalog in [false, true] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let original = existing_catalog.then(|| fixture.write_profiles(2, &[saved_openconnect()]));
    for action in ["create", "start"] {
      let output = fixture.output(&["vpn", action]).await;
      assert!(!output.status.success(), "{output:?}");
      assert!(output.stdout.is_empty(), "{output:?}");
      assert!(!output.stderr.is_empty(), "{output:?}");
      assert_no_saved_credentials(&output);
      if let Some(original) = &original {
        assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), *original);
      } else {
        assert!(!fixture.profiles_path().exists());
      }
      assert_no_daemon_contact(&listener).await;
    }
  }
}

#[tokio::test]
async fn remove_requires_a_terminal_and_has_no_confirmation_bypass_flag() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect()]);
  for args in [
    vec!["vpn", "remove"],
    vec!["vpn", "remove", "Work VPN"],
    vec!["vpn", "remove", "work-id", "--json"],
    vec!["vpn", "remove", "work-id", "--yes"],
  ] {
    let output = fixture.output(&args).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!output.stderr.is_empty(), "{output:?}");
    assert_no_saved_credentials(&output);
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert_no_daemon_contact(&listener).await;
  }
}

#[tokio::test]
async fn remove_rejects_unknown_and_ambiguous_saved_selectors_without_prompting_or_ipc() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut second = saved_openconnect();
  second["connection_id"] = "second-id".into();
  let original = fixture.write_profiles(2, &[saved_openconnect(), second]);
  for selector in ["unknown-profile", "Work VPN"] {
    let mut terminal = Terminal::new(&fixture, &["vpn", "remove", selector]);
    assert!(!terminal.finish().success());
    let transcript = String::from_utf8_lossy(&terminal.transcript);
    assert!(!transcript.contains("Remove VPN profile"));
    assert!(!transcript.contains("Choose a VPN"));
    assert!(!transcript.contains("saved-private-secret"));
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert_no_daemon_contact(&listener).await;
  }
}

#[tokio::test]
async fn remove_confirmation_defaults_to_no_and_cancellation_leaves_profiles_unchanged_without_ipc()
{
  for keys in ["\r", "\u{3}"] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let original = fixture.write_profiles(2, &[saved_openconnect()]);
    let mut terminal = Terminal::new(&fixture, &["vpn", "remove", "Work VPN"]);
    terminal.wait_for("Remove VPN profile");
    terminal.send(keys);
    let status = terminal.finish();
    assert!(
      status.success(),
      "terminal exited with {status:?} after {keys:?}: {}",
      String::from_utf8_lossy(&terminal.transcript)
    );
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert_no_daemon_contact(&listener).await;
  }
}

#[tokio::test]
async fn cancelling_remove_picker_keeps_saved_profiles_without_contacting_ctld() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "remove"]);
  terminal.wait_for("Choose a VPN to remove");
  terminal.send("\u{1b}[B\u{1b}");
  terminal.wait_for("Cancelled. No changes made.");
  assert!(terminal.finish().success());
  assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_remove_selects_saved_names_ids_or_picker_and_returns_only_removed_metadata() {
  for selector in [Some("work-id"), Some("Work VPN"), None] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let mut remaining = saved_tailscale();
    if selector == Some("work-id") {
      remaining["name"] = "work-id".into();
    }
    let original = fixture.write_profiles(2, &[remaining.clone(), saved_openconnect()]);
    let args = selector.map_or_else(
      || vec!["vpn", "remove", "--json"],
      |selector| vec!["vpn", "remove", selector, "--json"],
    );
    let mut terminal = Terminal::new(&fixture, &args);
    if selector.is_none() {
      terminal.wait_for("Choose a VPN to remove");
      terminal.send("\u{1b}[B\r");
    }
    terminal.wait_for("Remove VPN profile Work VPN (work-id)?");
    assert_no_daemon_contact(&listener).await;
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    let server = tokio::spawn(async move {
      let request = reply(&listener, response(VpnStatus::default())).await;
      (request, listener)
    });
    terminal.send("y");
    assert!(terminal.finish().success());
    let (request, listener) = timeout(Duration::from_secs(5), server)
      .await
      .unwrap()
      .unwrap();
    assert!(matches!(request, ClientMessage::VpnStatus));
    assert_no_daemon_contact(&listener).await;
    let transcript = String::from_utf8_lossy(&terminal.transcript);
    let removed: serde_json::Value = transcript
      .lines()
      .rev()
      .find_map(|line| serde_json::from_str(&line[line.find('{')?..]).ok())
      .expect("remove must print its JSON result after confirmation");
    assert_eq!(
      removed,
      serde_json::json!({
        "removed": true,
        "connection_id": "work-id",
        "name": "Work VPN",
        "provider": "openconnect",
      })
    );
    assert!(!transcript.contains("saved-private-secret"));
    assert!(!transcript.contains("saved-private-token"));
    let document: serde_json::Value =
      serde_json::from_slice(&std::fs::read(fixture.profiles_path()).unwrap()).unwrap();
    assert_eq!(document["connections"], serde_json::json!([remaining]));
    assert_eq!(
      std::fs::metadata(fixture.profiles_path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_remove_refuses_active_shared_or_unverifiable_inventory_without_stopping_a_vpn() {
  for scenario in [
    "owned",
    "shared",
    "starting",
    "unavailable",
    "other_provider",
    "warnings",
    "legacy",
  ] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let original = fixture.write_profiles(2, &[saved_openconnect()]);
    let mut active = VpnStatus {
      vpn_id: Some("runtime-work".into()),
      connection_id: Some("work-id".into()),
      locally_connected: Some(true),
      ..connected()
    };
    match scenario {
      "shared" => {
        active.shared_container = true;
        active.locally_connected = Some(false);
      }
      "starting" => {
        active.state = VpnState::Starting;
        active.running = false;
      }
      "unavailable" => {
        active.state = VpnState::Stopped;
        active.running = false;
        active.status_unavailable = true;
      }
      "other_provider" => active.provider = ctl_ipc::VpnProvider::Tailscale,
      _ => {}
    }
    let snapshot = VpnSnapshot {
      connections: if scenario == "warnings" {
        Vec::new()
      } else {
        vec![active.clone()]
      },
      supports_multiple: true,
      discovery_warnings: if scenario == "warnings" {
        vec!["Partial container inventory".into()]
      } else {
        Vec::new()
      },
      ..VpnSnapshot::default()
    };
    let response = if scenario == "legacy" {
      ServerMessage::VpnStatus {
        status: Box::new(VpnStatus::default()),
        snapshot: None,
      }
    } else {
      ServerMessage::VpnStatus {
        status: Box::new(active),
        snapshot: Some(snapshot),
      }
    };
    let mut terminal = Terminal::new(&fixture, &["vpn", "remove", "Work VPN"]);
    terminal.wait_for("Remove VPN profile");
    let server = tokio::spawn(async move {
      let request = reply(&listener, response).await;
      (request, listener)
    });
    terminal.send("y");
    assert!(!terminal.finish().success());
    let (request, listener) = timeout(Duration::from_secs(5), server)
      .await
      .unwrap()
      .unwrap();
    assert!(matches!(request, ClientMessage::VpnStatus));
    assert_no_daemon_contact(&listener).await;
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
    assert!(!String::from_utf8_lossy(&terminal.transcript).contains("saved-private-secret"));
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_remove_preserves_catalog_changes_made_while_the_confirmation_was_open() {
  for deleted in [false, true] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
    let mut terminal = Terminal::new(&fixture, &["vpn", "remove", "Work VPN"]);
    terminal.wait_for("Remove VPN profile");
    let current = if deleted {
      fixture.write_profiles(2, &[saved_tailscale()])
    } else {
      let mut changed = saved_openconnect();
      changed["password"] = "updated-private-password".into();
      fixture.write_profiles(2, &[changed, saved_tailscale()])
    };
    let server = tokio::spawn(async move {
      let request = reply(&listener, response(VpnStatus::default())).await;
      (request, listener)
    });
    terminal.send("y");
    assert!(!terminal.finish().success());
    let (request, listener) = timeout(Duration::from_secs(5), server)
      .await
      .unwrap()
      .unwrap();
    assert!(matches!(request, ClientMessage::VpnStatus));
    assert_no_daemon_contact(&listener).await;
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), current);
    let transcript = String::from_utf8_lossy(&terminal.transcript);
    assert!(!transcript.contains("updated-private-password"));
    assert!(!transcript.contains("saved-private-secret"));
  }
}

#[tokio::test]
async fn create_openconnect_in_a_terminal_masks_password_reprompts_and_only_saves_a_profile() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut terminal = Terminal::new(&fixture, &["vpn", "create"]);
  terminal.wait_for("VPN name");
  terminal.send("New work VPN\r");
  terminal.wait_for("VPN provider");
  terminal.send("\r");
  terminal.wait_for("VPN server");
  terminal.send("https://new-vpn.example.test/engineering\r");
  terminal.wait_for("Username");
  terminal.send("new-test-user\r");
  terminal.wait_for("Password");
  terminal.send("\r");
  terminal.wait_for("Input required");
  terminal.send("new-pty-private-password\r");
  terminal.wait_for("Authentication group (optional)");
  terminal.send("engineering\r");
  terminal.wait_for("Connectivity check IPv4 address (optional)");
  terminal.send("10.40.0.1\r");
  terminal.wait_for("VPN profile saved.");
  assert!(terminal.finish().success());
  let transcript = String::from_utf8_lossy(&terminal.transcript);
  assert!(!transcript.contains("new-pty-private-password"));
  let value: serde_json::Value =
    serde_json::from_slice(&std::fs::read(fixture.profiles_path()).unwrap()).unwrap();
  assert_eq!(value["schema_version"], 2);
  assert_eq!(value["connections"].as_array().unwrap().len(), 1);
  let profile = &value["connections"][0];
  uuid::Uuid::parse_str(profile["connection_id"].as_str().unwrap()).unwrap();
  assert_eq!(profile["name"], "New work VPN");
  assert_eq!(profile["provider"], "openconnect");
  assert_eq!(profile["url"], "https://new-vpn.example.test/engineering");
  assert_eq!(profile["username"], "new-test-user");
  assert_eq!(profile["password"], "new-pty-private-password");
  assert_eq!(profile["auth_method"], "engineering");
  assert_eq!(profile["target_ip"], "10.40.0.1");
  assert_eq!(
    std::fs::metadata(fixture.profiles_path())
      .unwrap()
      .permissions()
      .mode()
      & 0o777,
    0o600
  );
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test]
async fn create_tailscale_in_a_terminal_saves_provider_settings_without_contacting_ctld() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect()]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "create"]);
  terminal.wait_for("VPN name");
  terminal.send("New tailnet\r");
  terminal.wait_for("VPN provider");
  terminal.send("\u{1b}[B\r");
  terminal.wait_for("Device name (optional)");
  terminal.send("ctl-pty\r");
  terminal.wait_for("Use advertised subnet routes?");
  terminal.send("y");
  terminal.wait_for("VPN profile saved.");
  assert!(terminal.finish().success());
  let value: serde_json::Value =
    serde_json::from_slice(&std::fs::read(fixture.profiles_path()).unwrap()).unwrap();
  let original: serde_json::Value = serde_json::from_slice(&original).unwrap();
  assert_eq!(value["connections"].as_array().unwrap().len(), 2);
  assert_eq!(value["connections"][0], original["connections"][0]);
  let profile = &value["connections"][1];
  uuid::Uuid::parse_str(profile["connection_id"].as_str().unwrap()).unwrap();
  assert_eq!(profile["name"], "New tailnet");
  assert_eq!(profile["provider"], "tailscale");
  assert_eq!(profile["hostname"], "ctl-pty");
  assert_eq!(profile["accept_routes"], true);
  assert!(profile.get("password").is_none());
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test]
async fn cancelling_create_during_password_input_keeps_saved_profiles_and_never_contacts_ctld() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect()]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "create"]);
  terminal.wait_for("VPN name");
  terminal.send("Cancelled VPN\r");
  terminal.wait_for("VPN provider");
  terminal.send("\r");
  terminal.wait_for("VPN server");
  terminal.send("https://new-vpn.example.test\r");
  terminal.wait_for("Username");
  terminal.send("cancel-test-user\r");
  terminal.wait_for("Password");
  terminal.send("unfinished-private-password\u{3}");
  terminal.wait_for("Cancelled. No changes made.");
  assert!(terminal.finish().success());
  assert!(!String::from_utf8_lossy(&terminal.transcript).contains("unfinished-private-password"));
  assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_picker_reloads_current_settings_for_the_selected_stable_id() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut selected = saved_openconnect();
  selected["connection_id"] = "second-id".into();
  selected["name"] = "Second VPN".into();
  fixture.write_profiles(2, &[saved_openconnect(), selected.clone()]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "start"]);
  terminal.wait_for("Choose a VPN to start");
  assert_no_daemon_contact(&listener).await;
  selected["name"] = "Renamed VPN".into();
  selected["password"] = "updated-pty-private-password".into();
  selected["url"] = "https://updated-vpn.example.test/group".into();
  let expected: ctl_ipc::VpnConnection = serde_json::from_value(selected.clone()).unwrap();
  let current = fixture.write_profiles(2, &[saved_openconnect(), selected]);
  let server = tokio::spawn(async move { reply(&listener, response(connected())).await });
  terminal.send("\u{1b}[B\r");
  assert!(terminal.finish().success());
  let request = timeout(Duration::from_secs(5), server)
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(
    request,
    ClientMessage::StartVpnConnection { connection } if connection == expected
  ));
  assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), current);
}

#[tokio::test]
async fn start_picker_rejects_a_deleted_selection_even_when_its_id_matches_another_saved_name() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let mut selected = saved_openconnect();
  selected["connection_id"] = "second-id".into();
  selected["name"] = "Second VPN".into();
  fixture.write_profiles(2, &[saved_openconnect(), selected]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "start"]);
  terminal.wait_for("Choose a VPN to start");
  let mut remaining = saved_openconnect();
  remaining["name"] = "second-id".into();
  let current = fixture.write_profiles(2, &[remaining]);
  terminal.send("\u{1b}[B\r");
  assert!(!terminal.finish().success());
  assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), current);
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test]
async fn cancelling_start_picker_does_not_start_a_highlighted_profile_or_change_saved_settings() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let original = fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
  let mut terminal = Terminal::new(&fixture, &["vpn", "start"]);
  terminal.wait_for("Choose a VPN to start");
  terminal.send("\u{1b}[B\u{1b}");
  terminal.wait_for("Cancelled. No changes made.");
  assert!(terminal.finish().success());
  assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_picker_excludes_shared_only_connections_and_targets_the_owned_selection() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  fixture.write_profiles(2, &[saved_openconnect(), saved_tailscale()]);
  let shared = VpnStatus {
    provider: ctl_ipc::VpnProvider::Tailscale,
    vpn_id: Some("shared-only-id".into()),
    connection_id: Some("tailnet-id".into()),
    locally_connected: Some(false),
    shared_container: true,
    ..connected()
  };
  let owned = VpnStatus {
    vpn_id: Some("work-id".into()),
    connection_id: Some("work-id".into()),
    locally_connected: Some(true),
    shared_container: true,
    ..connected()
  };
  let snapshot = VpnSnapshot {
    connections: vec![shared, owned.clone()],
    supports_multiple: true,
    ..VpnSnapshot::default()
  };
  let server = tokio::spawn(async move {
    for _ in 0..2 {
      assert!(matches!(
        reply(
          &listener,
          ServerMessage::VpnStatus {
            status: Box::new(owned.clone()),
            snapshot: Some(snapshot.clone()),
          }
        )
        .await,
        ClientMessage::VpnStatus
      ));
    }
    reply(&listener, response(VpnStatus::default())).await
  });
  let mut terminal = Terminal::new(&fixture, &["vpn", "stop"]);
  // The heading and option can arrive in separate PTY reads.
  terminal.wait_for("Work VPN");
  let menu = String::from_utf8_lossy(&terminal.transcript);
  assert!(menu.contains("Work VPN"));
  assert!(!menu.contains("Team VPN"));
  assert!(!menu.contains("shared-only-id"));
  terminal.send("\r");
  assert!(terminal.finish().success());
  let request = timeout(Duration::from_secs(5), server)
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(request, ClientMessage::StopVpnById { vpn_id } if vpn_id == "work-id"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_stop_uses_untargeted_stop_for_a_legacy_local_daemon() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let server = tokio::spawn(async move {
    assert!(matches!(
      reply(
        &listener,
        ServerMessage::VpnStatus {
          status: Box::new(VpnStatus {
            vpn_id: None,
            ..connected()
          }),
          snapshot: None,
        }
      )
      .await,
      ClientMessage::VpnStatus
    ));
    reply(
      &listener,
      ServerMessage::VpnStatus {
        status: Box::new(VpnStatus::default()),
        snapshot: None,
      },
    )
    .await
  });
  let mut terminal = Terminal::new(&fixture, &["vpn", "stop"]);
  assert!(terminal.finish().success());
  let request = timeout(Duration::from_secs(5), server)
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(request, ClientMessage::StopVpn));
  let transcript = String::from_utf8_lossy(&terminal.transcript);
  assert!(!transcript.contains("Choose a VPN to stop"));
  assert!(transcript.contains("disconnected"));
  assert!(!fixture.profiles_path().exists());
}

#[tokio::test]
async fn start_selects_saved_name_or_id_and_sends_the_full_unchanged_profile() {
  for (schema_version, selector) in [(1, "Work VPN"), (2, "work-id")] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let mut profile = saved_openconnect();
    if schema_version == 1 {
      profile.as_object_mut().unwrap().remove("provider");
    }
    let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
    let original = fixture.write_profiles(schema_version, &[profile]);
    let ready = VpnStatus {
      vpn_id: Some("work-id".into()),
      connection_id: Some("work-id".into()),
      ..connected()
    };
    let (output, request) = exchange(
      &fixture,
      &listener,
      &["vpn", "start", selector, "--json"],
      response(ready.clone()),
    )
    .await;
    assert_json_status(&output, &ready);
    assert!(matches!(
      request,
      ClientMessage::StartVpnConnection { connection } if connection == expected
    ));
    assert_eq!(std::fs::read(fixture.profiles_path()).unwrap(), original);
  }
}

#[tokio::test]
async fn start_prefers_an_exact_id_over_another_profiles_matching_name() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let selected = saved_openconnect();
  let mut other = selected.clone();
  other["connection_id"] = "other-id".into();
  other["name"] = "work-id".into();
  fixture.write_profiles(2, &[other, selected.clone()]);
  let expected: ctl_ipc::VpnConnection = serde_json::from_value(selected).unwrap();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "start", "work-id"],
    response(connected()),
  )
  .await;
  assert!(output.status.success(), "{output:?}");
  assert!(String::from_utf8_lossy(&output.stdout).contains("socks5h://127.0.0.1:43210"));
  assert!(matches!(
    request,
    ClientMessage::StartVpnConnection { connection } if connection == expected
  ));
}

#[tokio::test]
async fn start_rejects_unknown_and_ambiguous_names_before_contacting_ctld() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let first = saved_openconnect();
  let mut second = first.clone();
  second["connection_id"] = "second-id".into();
  fixture.write_profiles(2, &[first, second]);
  for selector in ["unknown-profile", "Work VPN"] {
    let output = fixture.output(&["vpn", "start", selector, "--json"]).await;
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_no_saved_credentials(&output);
    let diagnostic = String::from_utf8_lossy(&output.stderr).to_lowercase();
    if selector == "Work VPN" {
      assert!(
        diagnostic.contains("ambiguous")
          || diagnostic.contains("multiple")
          || diagnostic.contains("more than one")
      );
    } else {
      assert!(
        diagnostic.contains("not found")
          || diagnostic.contains("not_found")
          || diagnostic.contains("no saved vpn")
      );
    }
    assert_no_daemon_contact(&listener).await;
  }

  let output = fixture.output(&["vpn", "stop", "Work VPN", "--json"]).await;
  assert!(!output.status.success(), "{output:?}");
  assert!(output.stdout.is_empty(), "{output:?}");
  assert_no_saved_credentials(&output);
  assert_no_daemon_contact(&listener).await;
}

#[tokio::test]
async fn saved_tailscale_start_checks_capabilities_and_preserves_provider_settings() {
  for supported in [true, false] {
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let profile = saved_tailscale();
    let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
    fixture.write_profiles(2, &[profile]);
    let pending = VpnStatus {
      provider: ctl_ipc::VpnProvider::Tailscale,
      vpn_id: Some("tailnet-id".into()),
      connection_id: Some("tailnet-id".into()),
      state: VpnState::Starting,
      auth_url: Some("https://login.tailscale.com/a/123abc".into()),
      ..VpnStatus::default()
    };
    let mut command = fixture.command(&["vpn", "start", "Team VPN", "--json"]);
    let server = async {
      assert!(matches!(
        reply(
          &listener,
          ServerMessage::VpnStatus {
            status: Box::new(VpnStatus::default()),
            snapshot: Some(VpnSnapshot {
              supported_providers: if supported {
                vec![
                  ctl_ipc::VpnProvider::Openconnect,
                  ctl_ipc::VpnProvider::Tailscale,
                ]
              } else {
                vec![ctl_ipc::VpnProvider::Openconnect]
              },
              ..VpnSnapshot::default()
            }),
          }
        )
        .await,
        ClientMessage::VpnStatus
      ));
      if supported {
        Some(reply(&listener, response(pending.clone())).await)
      } else {
        None
      }
    };
    let (output, request) = timeout(Duration::from_secs(5), async {
      tokio::join!(command.output(), server)
    })
    .await
    .unwrap();
    let output = output.unwrap();
    if supported {
      assert_json_status(&output, &pending);
      assert!(matches!(
        request,
        Some(ClientMessage::StartVpnConnection { connection }) if connection == expected
      ));
    } else {
      assert!(request.is_none());
      assert!(!output.status.success(), "{output:?}");
      assert!(output.stdout.is_empty(), "{output:?}");
      assert!(String::from_utf8_lossy(&output.stderr).contains("update and restart ctld"));
      assert_no_daemon_contact(&listener).await;
    }
  }
}

#[tokio::test]
async fn list_preserves_runtime_when_saved_files_are_unsafe_and_start_rejects_them_before_ipc() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  for invalid_file in ["permissions", "symlink", "malformed"] {
    let original = fixture.write_profiles(2, &[saved_openconnect()]);
    let path = fixture.profiles_path();
    match invalid_file {
      "permissions" => {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
      }
      "symlink" => {
        let target = fixture.directory.join("real-vpns.json");
        std::fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(target, &path).unwrap();
      }
      "malformed" => {
        fixture
          .write_profile_bytes(br#"{"schema_version":"saved-private-secret","connections":[]}"#);
      }
      _ => unreachable!(),
    }
    let ready = connected();
    let (output, request) = exchange(
      &fixture,
      &listener,
      &["vpn", "list", "--json"],
      response(ready.clone()),
    )
    .await;
    assert!(matches!(request, ClientMessage::VpnStatus));
    let value = inventory(&output);
    assert_eq!(value["connections"], serde_json::json!([ready]));
    assert_eq!(value["entries"].as_array().unwrap().len(), 1);
    assert_eq!(value["entries"][0]["saved"], false);
    assert_eq!(value["entries"][0]["state"], "connected");
    assert_ne!(
      value["profile_warnings"].as_array().unwrap().as_slice(),
      &[] as &[serde_json::Value]
    );
    assert_no_saved_credentials(&output);

    let output = fixture.output(&["vpn", "start", "work-id", "--json"]).await;
    assert!(!output.status.success(), "{invalid_file}: {output:?}");
    assert!(output.stdout.is_empty(), "{invalid_file}: {output:?}");
    assert!(!output.stderr.is_empty(), "{invalid_file}: {output:?}");
    assert_no_saved_credentials(&output);
    assert_no_daemon_contact(&listener).await;
    if invalid_file != "malformed" {
      assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    if invalid_file == "permissions" {
      assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
      );
    }
    std::fs::remove_file(path).unwrap();
  }
}
