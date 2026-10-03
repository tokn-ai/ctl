#![cfg(unix)]

use ctl_ipc::{GatewayKind, SshGateway, SshGatewayMode};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt as _;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = PathBuf::from("/tmp").join(format!("ctld-proxy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
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
async fn nested_ssh_proxy_uses_the_executing_helper_without_rediscovery() {
  let fixture = Fixture::new();
  // Spaces and quotes exercise the shell escaping in OpenSSH's ProxyCommand.
  let executable = fixture.0.join("selected ' ctld");
  std::fs::copy(env!("CARGO_BIN_EXE_ctld"), &executable).unwrap();
  let executable = executable.canonicalize().unwrap();
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
    assert!(!fixture.0.join(".tokn").exists());
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
  let fixture = Fixture::new();
  let executable = fixture.0.join("selected ' ctld");
  std::fs::copy(env!("CARGO_BIN_EXE_ctld"), &executable).unwrap();
  let executable = executable.canonicalize().unwrap();
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
    .env("CTLD_BIN", fixture.0.join("wrong-helper"))
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
  assert!(arguments.contains("ControlPath=none"), "{arguments}");
  let escaped = executable.to_string_lossy().replace('\'', "'\\''");
  assert!(
    arguments.contains(&format!(
      "ProxyCommand='{escaped}' --proxy-route {} --proxy-host %h --proxy-port %p",
      encoded(&prefix)
    )),
    "{arguments}"
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
  assert!(!fixture.0.join(".tokn").exists());
}
