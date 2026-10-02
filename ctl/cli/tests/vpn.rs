#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ctl_ipc::{ClientMessage, ServerMessage, VpnSnapshot, VpnState, VpnStatus};
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

async fn reply(listener: &UnixListener, response: ServerMessage) -> ClientMessage {
  let (mut stream, _) = listener.accept().await.unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
    Some(ClientMessage::Handshake { protocol_version })
      if protocol_version == ctl_ipc::PROTOCOL_VERSION
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
  let ready = connected();
  let (output, request) = exchange(
    &fixture,
    &listener,
    &["vpn", "start", "--env-file", "work.env", "--json"],
    response(ready.clone()),
  )
  .await;
  assert_json_status(&output, &ready);
  assert!(matches!(
    request,
    ClientMessage::StartVpn { env_file } if env_file == fixture.directory.join("work.env")
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
  for action in ["start", "list"] {
    let (output, request) =
      exchange(&fixture, &listener, &["vpn", action], response(connected())).await;
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
  let pending = VpnStatus {
    provider: ctl_ipc::VpnProvider::Tailscale,
    vpn_id: Some("team".into()),
    connection_id: Some("team".into()),
    state: VpnState::Starting,
    auth_url: Some("https://login.tailscale.com/a/123abc".into()),
    ..VpnStatus::default()
  };
  let mut command = fixture.command(&[
    "vpn",
    "start-tailscale",
    "--id",
    "team",
    "--hostname",
    "ctmux-test",
    "--accept-routes",
  ]);
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
  assert!(text.contains("Sign in for team: https://login.tailscale.com/a/123abc"));
  assert!(
    matches!(request, ClientMessage::StartVpnConnection { connection } if connection.connection_id == "team"
    && matches!(&connection.settings, ctl_ipc::VpnSettings::Tailscale { hostname: Some(hostname), accept_routes: true } if hostname == "ctmux-test"))
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
    let (output, _) = exchange(
      &fixture,
      &listener,
      &["vpn", "start"],
      response(VpnStatus {
        provider,
        vpn_id: Some("pending-vpn".into()),
        state: VpnState::Starting,
        auth_url: Some(auth_url.into()),
        ..VpnStatus::default()
      }),
    )
    .await;
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
async fn missing_daemon_reports_unavailable_inventory_without_starting_and_remote_commands_are_rejected()
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
  for action in ["start", "list", "stop", "connect"] {
    for json in [false, true] {
      let mut args = vec!["--host", "vpn-host", "vpn", action];
      if action == "connect" {
        args.push("work-id");
      }
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
async fn start_and_saved_connect_launch_ctld_when_absent() {
  for action in ["start", "connect"] {
    let fixture = Fixture::new();
    let profile = saved_openconnect();
    let expected: ctl_ipc::VpnConnection = serde_json::from_value(profile.clone()).unwrap();
    fixture.write_profiles(2, &[profile]);
    let helper = fixture.directory.join("ctld");
    let marker = fixture.directory.join("started");
    std::fs::write(
      &helper,
      format!(
        "#!/bin/sh\nif [ \"$1\" = --protocol-version ]; then\n  printf '%s\\n' {}\nelse\n  touch \"$CTL_VPN_TEST_MARKER\"\nfi\n",
        ctl_ipc::PROTOCOL_VERSION
      ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let args = if action == "connect" {
      vec!["vpn", action, "work-id"]
    } else {
      vec!["vpn", action]
    };
    let mut command = fixture.command(&args);
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
    if action == "connect" {
      assert!(matches!(
        request,
        ClientMessage::StartVpnConnection { connection } if connection == expected
      ));
    } else {
      assert!(matches!(
        request,
        ClientMessage::StartVpn { env_file } if env_file == fixture.directory.join(".env")
      ));
    }
  }
}

#[tokio::test]
async fn daemon_errors_are_reported_without_success_output() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  for args in [vec!["vpn", "start"], vec!["vpn", "start", "--json"]] {
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
    assert!(matches!(request, ClientMessage::StartVpn { .. }));
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
    for action in ["list", "start", "connect", "stop"] {
      let args = if action == "connect" {
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
    .output(&["vpn", "connect", "missing-profile", "--json"])
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
async fn connect_selects_saved_name_or_id_and_sends_the_full_unchanged_profile() {
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
      &["vpn", "connect", selector, "--json"],
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
async fn connect_prefers_an_exact_id_over_another_profiles_matching_name() {
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
    &["vpn", "connect", "work-id"],
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
async fn connect_rejects_unknown_and_ambiguous_names_before_contacting_ctld() {
  let fixture = Fixture::new();
  let listener = UnixListener::bind(fixture.socket()).unwrap();
  let first = saved_openconnect();
  let mut second = first.clone();
  second["connection_id"] = "second-id".into();
  fixture.write_profiles(2, &[first, second]);
  for selector in ["unknown-profile", "Work VPN"] {
    let output = fixture
      .output(&["vpn", "connect", selector, "--json"])
      .await;
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
}

#[tokio::test]
async fn saved_tailscale_connect_checks_capabilities_and_preserves_provider_settings() {
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
    let mut command = fixture.command(&["vpn", "connect", "Team VPN", "--json"]);
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
async fn list_preserves_runtime_when_saved_files_are_unsafe_and_connect_rejects_them_before_ipc() {
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

    let output = fixture
      .output(&["vpn", "connect", "work-id", "--json"])
      .await;
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
