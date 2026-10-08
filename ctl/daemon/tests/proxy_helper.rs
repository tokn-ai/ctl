#![cfg(unix)]

use ctl_ipc::{ClientMessage, GatewayKind, ServerMessage, SshGateway, SshGatewayMode, SshTarget};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct BrokerFixture {
  socket_path: PathBuf,
  task: tokio::task::JoinHandle<SshTarget>,
}

impl BrokerFixture {
  fn new(fixture: &Fixture, prepared_owner: SshTarget, response: ServerMessage) -> Self {
    let socket_path = fixture.0.join("broker.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let task = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      assert!(matches!(
        ctl_ipc::read_frame(&mut stream).await.unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctl_ipc::write_frame(
        &mut stream,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctl_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      let Some(ClientMessage::MasterStatus { target }) =
        ctl_ipc::read_frame(&mut stream).await.unwrap()
      else {
        panic!("A proxy must only query its owner's prepared master");
      };
      let response = if target == prepared_owner {
        response
      } else {
        ServerMessage::AuthenticationRequired
      };
      ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
      target
    });
    Self { socket_path, task }
  }

  async fn requested_owner(&mut self) -> SshTarget {
    tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task)
      .await
      .unwrap()
      .unwrap()
  }
}

impl Drop for BrokerFixture {
  fn drop(&mut self) {
    self.task.abort();
  }
}

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = PathBuf::from("/tmp").join(format!("ctld-proxy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn selected_executable(&self, guard: &ctl_core::test_fixtures::ProcessGuard) -> PathBuf {
    // Spaces and quotes exercise shell escaping in OpenSSH's ProxyCommand.
    let executable = self.0.join("selected ' ctld");
    guard.copy(env!("CARGO_BIN_EXE_ctld"), &executable).unwrap();
    executable.canonicalize().unwrap()
  }

  fn assert_only_history(&self) {
    // Proxying must not rediscover/install another helper or component store.
    assert_eq!(
      std::fs::read_dir(self.0.join(".tokn/ctl"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>(),
      [std::ffi::OsString::from("history")]
    );
  }

  fn command(
    &self,
    route: &[SshGateway],
    broker_socket: &std::path::Path,
  ) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"));
    command
      .args([
        "--proxy-route",
        &encoded(route),
        "--proxy-host",
        "private.internal",
        "--proxy-port",
        "2200",
      ])
      .env("PATH", &self.0)
      .env("HOME", &self.0)
      .env("CTLD_SOCKET_PATH", broker_socket)
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS_SOCKET")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .kill_on_drop(true);
    command
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn gateway(kind: GatewayKind, destination: &str) -> SshGateway {
  SshGateway {
    kind,
    vpn: None,
    destination: destination.into(),
    hostname: None,
    user: None,
    port: Some(1080),
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  }
}

fn encoded(route: &[SshGateway]) -> String {
  serde_json::to_vec(route)
    .unwrap()
    .iter()
    .fold(String::new(), |mut value, byte| {
      write!(value, "{byte:02x}").unwrap();
      value
    })
}

#[tokio::test]
async fn an_explicit_broker_socket_is_pinned_on_its_ssh_children() {
  let _fixture_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
  let socket = fixture.0.join("owner.sock");
  let marker = fixture.0.join("ssh-broker-socket");
  let ssh = fixture.0.join("ssh");
  std::fs::write(
    &ssh,
    concat!(
      "#!/bin/sh\n",
      "for arg in \"$@\"; do\n",
      "  if [ \"$arg\" = -M ]; then\n",
      "    printf '%s' \"$CTLD_SOCKET_PATH\" > \"$CTLD_TEST_BROKER_SOCKET\"\n",
      "    exit 1\n",
      "  fi\n",
      "done\n",
      "exit 1\n",
    ),
  )
  .unwrap();
  std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
  let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--socket")
    .arg(&socket)
    .env("PATH", &fixture.0)
    .env("HOME", &fixture.0)
    // This may happen when a broker is launched directly rather than through
    // connect(). Its children must still address the socket actually bound.
    .env("CTLD_SOCKET_PATH", fixture.0.join("unrelated.sock"))
    .env("CTLD_TEST_BROKER_SOCKET", &marker)
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  tokio::time::timeout(std::time::Duration::from_secs(5), async {
    let mut broker = loop {
      if let Ok(stream) = ctl_ipc::connect_existing_at(&socket).await {
        break stream;
      }
      if let Some(status) = child.try_wait().unwrap() {
        let mut diagnostics = String::new();
        child
          .stderr
          .take()
          .unwrap()
          .read_to_string(&mut diagnostics)
          .await
          .unwrap();
        panic!("broker exited early ({status}): {diagnostics}");
      }
      tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    ctl_ipc::write_frame(
      &mut broker,
      &ClientMessage::Handshake {
        protocol: ctl_ipc::protocol_offer(),
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, ServerMessage>(&mut broker)
        .await
        .unwrap(),
      Some(ServerMessage::HandshakeAccepted { .. })
    ));
    ctl_ipc::write_frame(
      &mut broker,
      &ClientMessage::EnsureMaster {
        target: SshTarget {
          ssh_config_alias: None,
          use_ssh_config_master: Some(false),
          destination: "fixture.invalid".into(),
          hostname: None,
          user: None,
          port: None,
          identity_file: None,
          gateways: vec![],
        },
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, ServerMessage>(&mut broker)
        .await
        .unwrap(),
      Some(ServerMessage::Error { .. })
    ));
    assert_eq!(
      std::fs::read_to_string(&marker).unwrap(),
      socket.to_str().unwrap()
    );
  })
  .await
  .unwrap();
  child.kill().await.unwrap();
}

#[tokio::test]
async fn nested_ssh_proxy_uses_the_executing_helper_without_rediscovery() {
  let fixture_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let executable = fixture.selected_executable(&fixture_guard);
  let ssh = fixture.0.join("ssh");
  std::fs::write(
    &ssh,
    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CTLD_TEST_PROXY_ARGS\"\nexit 0\n",
  )
  .unwrap();
  std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
  let prefix = [gateway(GatewayKind::Socks5, "socks.example.invalid")];
  let mut jump = gateway(GatewayKind::Ssh, "saved-jump-alias");
  jump.hostname = Some("jump.example.invalid".into());
  let route = [prefix[0].clone(), jump];
  let marker = fixture.0.join("ssh-arguments");
  for explicit_override in [false, true] {
    let mut command = tokio::process::Command::new(&executable);
    command
      .args([
        "--proxy-route",
        &encoded(&route),
        "--proxy-host",
        "target.example.invalid",
        "--proxy-port",
        "2222",
      ])
      .env("PATH", &fixture.0)
      .env("HOME", &fixture.0)
      .env("HOME", &fixture.0)
      .env("CTLD_TEST_PROXY_ARGS", &marker)
      .env_remove("CTLD_BIN")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS_SOCKET")
      .stdin(Stdio::null())
      .kill_on_drop(true);
    if explicit_override {
      command.env("CTLD_BIN", fixture.0.join("unrelated-ctld"));
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
      .await
      .unwrap()
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_eq!(output.stderr, Vec::<u8>::new());
    let arguments = std::fs::read_to_string(&marker).unwrap();
    for required in [
      "saved-jump-alias",
      "HostName=jump.example.invalid",
      "ForkAfterAuthentication=no",
      "StdinNull=no",
    ] {
      assert!(
        arguments.lines().any(|argument| argument == required),
        "{arguments}"
      );
    }
    let escaped = executable.to_string_lossy().replace('\'', "'\\''");
    let expected = format!(
      "ProxyCommand='{escaped}' --proxy-route {} --proxy-host %h --proxy-port %p",
      encoded(&prefix)
    );
    assert!(
      arguments.lines().any(|argument| argument == expected),
      "{arguments}"
    );
    fixture.assert_only_history();
  }
}

fn remote_vpn_bridge(fixture: &Fixture) -> PathBuf {
  let identity = serde_json::json!({
    "remote_id": "11111111-1111-4111-8111-111111111111", "agent_version": "0.1.0",
    "protocols": ctl_proto::agent_protocols()
  });
  let mut response = ctl_ipc::remote_vpn::PREFACE.to_vec();
  for frame in [
    serde_json::to_value(ctl_ipc::remote_vpn::protocol_offer()).unwrap(),
    identity,
    serde_json::json!({"type": "connected"}),
  ] {
    let bytes = serde_json::to_vec(&frame).unwrap();
    response.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
    response.extend_from_slice(&bytes);
  }
  response.extend_from_slice(b"remote-vpn-output");
  let response_path = fixture.0.join("response");
  std::fs::write(&response_path, response).unwrap();
  let ssh = fixture.0.join("ssh");
  std::fs::write(
    &ssh,
    concat!(
      "#!/bin/sh\n",
      "printf '%s\\n' \"$@\" > \"$CTLD_TEST_PROXY_ARGS\"\n",
      "/bin/cat \"$CTLD_TEST_REMOTE_RESPONSE\"\n",
      "/bin/cat > \"$CTLD_TEST_REMOTE_REQUEST\"\n",
    ),
  )
  .unwrap();
  std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
  response_path
}

#[tokio::test]
async fn remote_vpn_uses_its_ssh_owner_and_preserves_the_remote_dns_destination() {
  let fixture_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let fixture = Fixture::new();
  let executable = fixture.selected_executable(&fixture_guard);
  let response_path = remote_vpn_bridge(&fixture);
  let prefix = [gateway(GatewayKind::Socks5, "proxy.example.invalid")];
  let mut owner = gateway(GatewayKind::Ssh, "jump-a");
  owner.user = Some("alice".into());
  owner.port = Some(2222);
  let vpn = SshGateway {
    kind: GatewayKind::Vpn,
    vpn: Some(ctl_ipc::VpnGateway {
      connection_id: "work".into(),
      socket_path: fixture.0.join("must-not-open-local-owner.sock"),
      expected_remote_id: Some("11111111-1111-4111-8111-111111111111".into()),
    }),
    destination: "work".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  };
  let route = [prefix[0].clone(), owner, vpn];
  let mut broker = BrokerFixture::new(
    &fixture,
    ctl_ipc::vpn_owner_target(&route, 2).unwrap(),
    ServerMessage::MasterReady {
      control_path: fixture.0.join("authenticated-owner"),
    },
  );
  let arguments_path = fixture.0.join("arguments");
  let request_path = fixture.0.join("request");
  let mut child = tokio::process::Command::new(&executable)
    .args([
      "--proxy-route",
      &encoded(&route),
      "--proxy-host",
      "private.internal",
      "--proxy-port",
      "2200",
    ])
    .env("PATH", &fixture.0)
    .env("HOME", &fixture.0)
    .env("HOME", &fixture.0)
    .env("CTLD_BIN", fixture.0.join("wrong-helper"))
    .env("CTLD_SOCKET_PATH", &broker.socket_path)
    .env("CTLD_TEST_PROXY_ARGS", &arguments_path)
    .env("CTLD_TEST_REMOTE_RESPONSE", &response_path)
    .env("CTLD_TEST_REMOTE_REQUEST", &request_path)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let mut input = child.stdin.take().unwrap();
  input.write_all(b"destination-input").await.unwrap();
  input.shutdown().await.unwrap();
  drop(input);
  let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
    .await
    .unwrap()
    .unwrap();
  assert!(output.status.success(), "{output:?}");
  assert_eq!(output.stdout, b"remote-vpn-output");
  assert!(output.stderr.is_empty(), "{output:?}");
  let arguments = std::fs::read_to_string(arguments_path).unwrap();
  assert!(
    arguments.lines().any(|argument| argument == "jump-a"),
    "{arguments}"
  );
  assert!(arguments.contains("ctl-agent vpn"), "{arguments}");
  assert!(arguments.contains("authenticated-owner"), "{arguments}");
  assert!(arguments.contains("ProxyCommand=false"), "{arguments}");
  assert!(arguments.contains("BatchMode=yes"), "{arguments}");
  assert!(!arguments.contains("ControlPath=none"), "{arguments}");
  assert_eq!(
    broker.requested_owner().await,
    ctl_ipc::vpn_owner_target(&route, 2).unwrap()
  );
  let request = std::fs::read(request_path).unwrap();
  let selected_size =
    usize::try_from(u32::from_be_bytes(request[..4].try_into().unwrap())).unwrap();
  let selection: serde_json::Value =
    serde_json::from_slice(&request[4..4 + selected_size]).unwrap();
  assert_eq!(
    selection,
    serde_json::json!({ "protocol_version": "1.0.1" })
  );
  let request = &request[4 + selected_size..];
  let size = usize::try_from(u32::from_be_bytes(request[..4].try_into().unwrap())).unwrap();
  let frame: serde_json::Value = serde_json::from_slice(&request[4..4 + size]).unwrap();
  assert_eq!(
    frame,
    serde_json::json!({
      "type": "connect", "connection_id": "work", "host": "private.internal", "port": 2200
    })
  );
  assert_eq!(&request[4 + size..], b"destination-input");
  fixture.assert_only_history();
}

fn vpn_route(fixture: &Fixture) -> (Vec<SshGateway>, SshTarget) {
  let prefix = gateway(GatewayKind::Socks5, "proxy.example.invalid");
  let mut hop = gateway(GatewayKind::Ssh, "saved-hop-alias");
  hop.hostname = Some("10.0.0.7".into());
  hop.user = Some("alice".into());
  hop.port = Some(2222);
  let owner = SshTarget {
    destination: "saved-hop-alias".into(),
    ssh_config_alias: Some("saved-hop-alias".into()),
    use_ssh_config_master: Some(false),
    hostname: Some("10.0.0.7".into()),
    user: Some("alice".into()),
    port: Some(2222),
    identity_file: None,
    gateways: vec![prefix.clone()],
  };
  let vpn = SshGateway {
    kind: GatewayKind::Vpn,
    vpn: Some(ctl_ipc::VpnGateway {
      connection_id: "work".into(),
      socket_path: fixture.0.join("unused-remote-owner.sock"),
      expected_remote_id: Some("11111111-1111-4111-8111-111111111111".into()),
    }),
    destination: "work".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  };
  (vec![prefix, hop, vpn], owner)
}

#[tokio::test]
async fn unavailable_or_different_vpn_owner_never_starts_ssh_or_authenticates() {
  let _fixture_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for scenario in [
    "missing_broker",
    "not_authenticated",
    "different_account",
    "disconnected",
  ] {
    let fixture = Fixture::new();
    remote_vpn_bridge(&fixture);
    let (mut route, owner) = vpn_route(&fixture);
    if scenario == "different_account" {
      route[1].user = Some("bob".into());
    }
    let response = if scenario == "disconnected" {
      ServerMessage::Error {
        code: "ssh_host_disconnected".into(),
        message: "The SSH host is disconnected".into(),
      }
    } else {
      ServerMessage::AuthenticationRequired
    };
    let mut broker =
      (scenario != "missing_broker").then(|| BrokerFixture::new(&fixture, owner, response));
    let socket = broker.as_ref().map_or_else(
      || fixture.0.join("absent.sock"),
      |broker| broker.socket_path.clone(),
    );
    let arguments_path = fixture.0.join("unexpected-ssh-arguments");
    let output = tokio::time::timeout(
      std::time::Duration::from_secs(5),
      fixture
        .command(&route, &socket)
        .env("CTLD_TEST_PROXY_ARGS", &arguments_path)
        .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!output.status.success(), "{scenario}: {output:?}");
    assert!(output.stdout.is_empty(), "{scenario}: {output:?}");
    assert!(
      !arguments_path.exists(),
      "{scenario}: started a fresh SSH channel"
    );
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
      !diagnostic.contains("update its components"),
      "{scenario}: {diagnostic}"
    );
    if let Some(broker) = &mut broker {
      let requested = broker.requested_owner().await;
      assert_eq!(
        requested.ssh_config_alias.as_deref(),
        Some("saved-hop-alias")
      );
      assert_eq!(
        requested.user.as_deref(),
        Some(if scenario == "different_account" {
          "bob"
        } else {
          "alice"
        })
      );
      assert!(!requested.uses_ssh_config_master());
    }
  }
}

#[tokio::test]
async fn ssh_and_remote_vpn_deliver_output_eof_before_input_closes() {
  let _fixture_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for remote_vpn in [false, true] {
    let fixture = Fixture::new();
    let response_path = remote_vpn_bridge(&fixture);
    let (route, owner) = vpn_route(&fixture);
    let mut broker = remote_vpn.then(|| {
      BrokerFixture::new(
        &fixture,
        owner.clone(),
        ServerMessage::MasterReady {
          control_path: fixture.0.join("prepared-owner"),
        },
      )
    });
    let route = if remote_vpn {
      route
    } else {
      vec![gateway(GatewayKind::Ssh, "fixture-hop")]
    };
    let socket = broker.as_ref().map_or_else(
      || fixture.0.join("unused.sock"),
      |broker| broker.socket_path.clone(),
    );
    let upload_path = fixture.0.join("late-upload");
    let ssh = fixture.0.join("ssh");
    std::fs::write(&ssh, concat!(
      "#!/bin/sh\n",
      "if [ \"$CTLD_TEST_VPN\" = true ]; then /bin/cat \"$CTLD_TEST_REMOTE_RESPONSE\"; else printf 'remote-vpn-output'; fi\n",
      "exec 1>/dev/null\n",
      "/bin/cat > \"$CTLD_TEST_UPLOAD\"\n",
    )).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut child = fixture
      .command(&route, &socket)
      .env("CTLD_TEST_VPN", remote_vpn.to_string())
      .env("CTLD_TEST_REMOTE_RESPONSE", response_path)
      .env("CTLD_TEST_UPLOAD", &upload_path)
      .spawn()
      .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(
      std::time::Duration::from_secs(3),
      output.read_to_end(&mut reply),
    )
    .await
    .expect("remote EOF must reach OpenSSH while helper stdin is open")
    .unwrap();
    assert_eq!(reply, b"remote-vpn-output");
    assert!(child.try_wait().unwrap().is_none());
    input.write_all(b"upload-after-output-eof").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
      loop {
        if std::fs::read(&upload_path)
          .is_ok_and(|upload| upload.ends_with(b"upload-after-output-eof"))
        {
          break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
      }
    })
    .await
    .expect("the remote reader must receive a late upload while input remains open");
    drop(input);
    assert!(
      tokio::time::timeout(std::time::Duration::from_secs(3), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success()
    );
    let upload = std::fs::read(&upload_path).unwrap();
    assert!(upload.ends_with(b"upload-after-output-eof"));
    if remote_vpn {
      assert_eq!(broker.as_mut().unwrap().requested_owner().await, owner);
    } else {
      assert_eq!(upload, b"upload-after-output-eof");
    }
  }
}
