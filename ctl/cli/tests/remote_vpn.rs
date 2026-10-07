#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use ctl_ipc::remote_vpn::{Request, Response};
use ctl_ipc::{ClientMessage, ServerMessage, VpnConnection, VpnSnapshot, VpnState, VpnStatus};
use serde::Serialize;
use serde_json::json;
use tokio::net::UnixListener;
use tokio::time::timeout;

const REMOTE_ID: &str = "67172550-cb67-4b81-b365-74c68d72d250";

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    let path = PathBuf::from("/tmp").join(format!("ctl-remote-vpn-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    fs::write(
      path.join("hosts.json"),
      serde_json::to_vec(&json!({
        "revision": "test",
      "document": {"schema_version": 1, "ssh_gateways": [], "hosts": [{
          "host_id": "jump", "name": "Gateway", "preferred_method_id": "ssh",
          "remote_info": {"remote_id": REMOTE_ID, "agent_version": "test"},
          "connection_methods": [{"method_id": "ssh", "name": "SSH", "target": {
            "kind": "ssh", "destination": "jump.example.test", "user": "remote-user", "port": 2200
          }}]
        }]}
      }))
      .unwrap(),
    )
    .unwrap();
    fs::write(
      path.join("vpns.json"),
      serde_json::to_vec(&json!({
        "schema_version": 2, "connections": [Self::connection()]
      }))
      .unwrap(),
    )
    .unwrap();
    fs::set_permissions(path.join("vpns.json"), fs::Permissions::from_mode(0o600)).unwrap();
    Self(path)
  }

  fn connection() -> VpnConnection {
    serde_json::from_value(json!({
      "connection_id": "work", "name": "Work VPN", "provider": "openconnect",
      "url": "https://vpn.example.test", "username": "vpn-user", "password": "private-test-password",
      "auth_method": null, "target_ip": null
    })).unwrap()
  }

  fn ssh(&self, request: &Request, response: &Response, remote_id: &str) {
    let identity = json!({"remote_id": remote_id, "agent_version": "test", "protocols": ctl_proto::agent_protocols()});
    let mut preface = ctl_ipc::remote_vpn::PREFACE.to_vec();
    preface.extend(frame(&ctl_ipc::remote_vpn::protocol_offer()));
    preface.extend(frame(&identity));
    fs::write(self.0.join("preface"), preface).unwrap();
    fs::write(self.0.join("response"), frame(response)).unwrap();
    let script = format!(
      "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CTL_TEST_ARGS\"\ncat \"$CTL_TEST_PREFACE\"\ndd bs=1 count={} of=\"$CTL_TEST_SELECTION\" 2>/dev/null\ndd bs=1 count={} of=\"$CTL_TEST_REQUEST\" 2>/dev/null\nexec cat \"$CTL_TEST_RESPONSE\"\n",
      frame(&ctl_ipc::remote_vpn::ProtocolSelection {
        protocol_version: ctl_ipc::remote_vpn::PROTOCOL_VERSION
      })
      .len(),
      frame(request).len(),
    );
    ctl_core::test_fixtures::shell_command(self.0.join("ssh"), script).unwrap();
  }

  fn broker(&self, count: usize) -> tokio::task::JoinHandle<Vec<ctl_ipc::SshTarget>> {
    self.broker_contract(count, ctl_ipc::PROTOCOL_VERSION)
  }

  fn broker_contract(
    &self,
    count: usize,
    protocol_version: ctl_core::protocol::ProtocolVersion,
  ) -> tokio::task::JoinHandle<Vec<ctl_ipc::SshTarget>> {
    let listener = UnixListener::bind(self.0.join("ctld.sock")).unwrap();
    let control_path = self.0.join("master");
    let previous_request = self.0.join("request");
    tokio::spawn(async move {
      let mut targets = Vec::new();
      for _ in 0..count {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
          ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
            .await
            .unwrap(),
          Some(ClientMessage::Handshake { protocol })
            if protocol.accepts(protocol_version)
        ));
        ctl_ipc::write_frame(
          &mut stream,
          &ServerMessage::HandshakeAccepted { protocol_version },
        )
        .await
        .unwrap();
        let target = match ctl_ipc::read_frame(&mut stream).await.unwrap() {
          Some(ClientMessage::EnsureMaster { target }) => target,
          None => continue, // Complete-route capability preflight never authenticates.
          _ => {
            panic!("remote VPN must authenticate the selected host, without local VPN operations")
          }
        };
        if !targets.is_empty() {
          let bytes = fs::read(&previous_request).unwrap();
          let previous: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
          assert_eq!(
            previous["type"], "start",
            "preceding VPN starts before the next SSH master"
          );
        }
        ctl_ipc::write_frame(
          &mut stream,
          &ServerMessage::MasterReady {
            control_path: control_path.clone(),
          },
        )
        .await
        .unwrap();
        targets.push(target);
      }
      targets
    })
  }

  async fn output(&self, args: &[&str]) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .args(["--host", "Gateway", "vpn"])
      .args(args)
      .env("CTL_HOSTS_PATH", self.0.join("hosts.json"))
      .env("CTL_VPNS_PATH", self.0.join("vpns.json"))
      .env("CTLD_SOCKET_PATH", self.0.join("ctld.sock"))
      .env("CTLD_BIN", self.0.join("must-not-start-ctld"))
      .env_remove("CTLD_VPN_SOCKET_PATH")
      .env(
        "PATH",
        format!("{}:{}", self.0.display(), std::env::var("PATH").unwrap()),
      )
      .env("CTL_TEST_ARGS", self.0.join("args"))
      .env("CTL_TEST_PREFACE", self.0.join("preface"))
      .env("CTL_TEST_RESPONSE", self.0.join("response"))
      .env("CTL_TEST_REQUEST", self.0.join("request"))
      .env("CTL_TEST_SELECTION", self.0.join("selection"))
      .kill_on_drop(true);
    timeout(Duration::from_secs(10), command.output())
      .await
      .unwrap()
      .unwrap()
  }

  fn chain_route(&self) {
    let catalog_path = self.0.join("hosts.json");
    let mut catalog: serde_json::Value =
      serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
    catalog["document"]["ssh_gateways"] = json!([{
      "gateway_id": "bastion", "name": "Bastion", "destination": "bastion.example.test",
      "user": "gateway-user", "remote_info": {"remote_id": REMOTE_ID, "agent_version": "test"}
    }]);
    catalog["document"]["hosts"][0]["connection_methods"][0]["target"]["gateway_route"] = json!([
      {"gateway_id": "bastion", "mode": "automatic"}, {"vpn_connection_id": "work"}
    ]);
    fs::write(catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
  }

  fn request(&self) -> serde_json::Value {
    let selection = fs::read(self.0.join("selection")).unwrap();
    let selected: ctl_ipc::remote_vpn::ProtocolSelection =
      serde_json::from_slice(&selection[4..]).unwrap();
    assert_eq!(
      selected.protocol_version,
      ctl_ipc::remote_vpn::PROTOCOL_VERSION
    );
    let bytes = fs::read(self.0.join("request")).unwrap();
    serde_json::from_slice(&bytes[4..]).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn frame(value: &impl Serialize) -> Vec<u8> {
  let bytes = serde_json::to_vec(value).unwrap();
  let length = u32::try_from(bytes.len()).unwrap();
  let mut frame = length.to_be_bytes().to_vec();
  frame.extend(bytes);
  frame
}

#[tokio::test]
async fn selected_host_vpn_start_list_and_stop_use_authenticated_remote_control() {
  for action in ["start", "list", "stop"] {
    let fixture = Fixture::new();
    if action != "start" {
      fixture.chain_route();
    }
    let status = VpnStatus {
      vpn_id: Some("work".into()),
      connection_id: Some("work".into()),
      endpoint: Some("socks5h://127.0.0.1:39000".into()),
      running: true,
      state: VpnState::Connected,
      locally_connected: Some(true),
      ..VpnStatus::default()
    };
    let request = match action {
      "start" => Request::Start {
        connection: Fixture::connection(),
      },
      "list" => Request::List,
      _ => Request::Stop {
        vpn_id: "work".into(),
      },
    };
    let response = if action == "list" {
      Response::Snapshot {
        snapshot: VpnSnapshot {
          connections: vec![status],
          ..VpnSnapshot::default()
        },
      }
    } else {
      Response::Status { status }
    };
    fixture.ssh(&request, &response, REMOTE_ID);
    let broker = fixture.broker(1);
    let args = if action == "list" {
      vec![action, "--json"]
    } else {
      vec![action, "Work VPN", "--json"]
    };
    let output = fixture.output(&args).await;
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    let target = broker.await.unwrap().remove(0);
    assert_eq!(target.destination, "jump.example.test");
    assert_eq!(target.user.as_deref(), Some("remote-user"));
    assert_eq!(target.port, Some(2200));
    assert_eq!(target.gateways.len(), if action == "start" { 0 } else { 2 });
    assert_eq!(fixture.request(), serde_json::to_value(&request).unwrap());
    let arguments = fs::read_to_string(fixture.0.join("args")).unwrap();
    assert!(arguments.contains("ProxyCommand=false"));
    assert!(arguments.contains("BatchMode=yes"));
    assert!(arguments.contains(fixture.0.join("master").to_str().unwrap()));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-test-password"));
    if action == "list" {
      let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
      assert_eq!(snapshot["entries"][0]["name"], "Work VPN");
      assert_eq!(
        snapshot["connections"][0]["endpoint"],
        "socks5h://127.0.0.1:39000"
      );
    }
  }
}

#[tokio::test]
async fn rejected_remote_start_never_authenticates_or_starts_route_vpns() {
  for args in [
    &["start", "missing-profile", "--json"][..],
    &["start", "--json"],
  ] {
    let fixture = Fixture::new();
    fixture.chain_route();
    let listener = UnixListener::bind(fixture.0.join("ctld.sock")).unwrap();
    let output = fixture.output(args).await;
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
      message.contains("missing-profile") || message.contains("interactive terminal"),
      "{message}"
    );
    assert!(
      timeout(Duration::from_millis(30), listener.accept())
        .await
        .is_err()
    );
  }
}

#[tokio::test]
async fn remote_route_vpn_starts_on_the_jump_before_connecting_the_selected_host() {
  let fixture = Fixture::new();
  fixture.chain_route();
  let request = Request::Start {
    connection: Fixture::connection(),
  };
  fixture.ssh(
    &request,
    &Response::Status {
      status: VpnStatus {
        vpn_id: Some("work".into()),
        connection_id: Some("work".into()),
        endpoint: Some("socks5h://127.0.0.1:39000".into()),
        running: true,
        state: VpnState::Connected,
        ..VpnStatus::default()
      },
    },
    REMOTE_ID,
  );
  let broker = fixture.broker(3);
  let output = fixture.output(&["start", "work", "--json"]).await;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let targets = broker.await.unwrap();
  assert_eq!(targets[0].destination, "bastion.example.test");
  assert_eq!(targets[0].user.as_deref(), Some("gateway-user"));
  assert_eq!(targets[0].gateways, [] as [ctl_ipc::SshGateway; 0]);
  assert_eq!(targets[1].destination, "jump.example.test");
  assert_eq!(targets[1].gateways.len(), 2);
  assert_eq!(targets[1].gateways[0].destination, "bastion.example.test");
  let vpn = targets[1].gateways[1].vpn.as_ref().unwrap();
  assert_eq!(vpn.connection_id, "work");
  assert_eq!(vpn.expected_remote_id.as_deref(), Some(REMOTE_ID));
  assert_eq!(fixture.request(), serde_json::to_value(&request).unwrap());
}

#[tokio::test]
async fn changed_remote_identity_prevents_sending_saved_vpn_credentials() {
  let fixture = Fixture::new();
  fixture.ssh(
    &Request::Start {
      connection: Fixture::connection(),
    },
    &Response::Status {
      status: VpnStatus::default(),
    },
    "1997c747-c7f6-444e-b528-7b872e29b9cd",
  );
  let broker = fixture.broker(1);
  let output = fixture.output(&["start", "work", "--json"]).await;
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("Remote identity changed"));
  assert!(!String::from_utf8_lossy(&output.stderr).contains("private-test-password"));
  assert_eq!(
    fs::read(fixture.0.join("request")).unwrap_or_default(),
    [] as [u8; 0]
  );
  broker.await.unwrap();
}

#[tokio::test]
async fn old_broker_rejects_remote_routes_before_authentication_or_prerequisite_startup() {
  for action in ["start", "list", "stop"] {
    let fixture = Fixture::new();
    fixture.chain_route();
    let broker = fixture.broker_contract(1, ctl_ipc::CONTRACT_V1_0_12);
    let args = if action == "list" {
      vec![action, "--json"]
    } else {
      vec![action, "work", "--json"]
    };
    let output = fixture.output(&args).await;
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
      message.contains("1.1.13") && message.contains("1.0.12"),
      "{message}"
    );
    assert_eq!(broker.await.unwrap(), [] as [ctl_ipc::SshTarget; 0]);
    assert!(
      !fixture.0.join("args").exists(),
      "unsupported routes must not start SSH or send VPN credentials"
    );
  }
}

#[tokio::test]
async fn old_broker_can_authenticate_an_ssh_only_owner_for_remote_vpn_management() {
  let fixture = Fixture::new();
  fixture.ssh(
    &Request::List,
    &Response::Snapshot {
      snapshot: VpnSnapshot::default(),
    },
    REMOTE_ID,
  );
  let broker = fixture.broker_contract(1, ctl_ipc::CONTRACT_V1_0_12);
  let output = fixture.output(&["list", "--json"]).await;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(broker.await.unwrap().len(), 1);
  assert_eq!(fixture.request(), json!({"type":"list"}));
}
