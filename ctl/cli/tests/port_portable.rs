//! Exercise the CLI forwarding boundary using the native local IPC transport.

use ctl_client::hosts::ConnectionTargetDto;
use ctl_ipc::{
  ClientMessage, LocalPortForward, PortForwardState, PortForwardStatus, ServerMessage,
};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(windows)]
use ctl_ipc::Listener;
#[cfg(unix)]
use tokio::net::UnixListener as Listener;

struct Fixture {
  directory: PathBuf,
  socket_path: PathBuf,
}

impl Fixture {
  fn new() -> Self {
    #[cfg(unix)]
    let root = PathBuf::from("/tmp");
    #[cfg(windows)]
    let root = std::env::temp_dir();
    let directory = root.join(format!("ctl-port-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    fs::write(
      directory.join("hosts.json"),
      serde_json::to_vec(&json!({
        "revision": "fixture", "document": {
          "schema_version": 1, "ssh_gateways": [], "hosts": [{
            "host_id": "saved-1", "name": "work", "preferred_method_id": "ssh",
            "connection_methods": [{
              "method_id": "ssh", "name": "Config", "ssh_config_alias": "configured",
              "target": {
                "kind": "ssh", "destination": "configured", "hostname": "192.0.2.20",
                "user": "saved-user", "port": 2222, "identity_file": "identity with spaces"
              }
            }]
          }]
        }
      }))
      .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    let socket_path = directory.join("ctld.sock");
    #[cfg(windows)]
    let socket_path = PathBuf::from(format!(r"\\.\pipe\ctl-port-{}", uuid::Uuid::new_v4()));
    Self {
      directory,
      socket_path,
    }
  }

  async fn run(&self, saved: bool, arguments: &[&str]) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctl"));
    command
      .env("CTL_HOSTS_PATH", self.directory.join("hosts.json"))
      .env("CTLD_SOCKET_PATH", &self.socket_path)
      .env("CTLD_BIN", self.directory.join("must-not-start-ctld"))
      .args(if saved {
        ["--host", "work", "port"]
      } else {
        ["--destination", "configured", "port"]
      })
      .args(arguments)
      .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
      .await
      .expect("forwarding command timed out")
      .unwrap();
    assert!(output.status.success(), "{output:?}");
    output
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.directory);
  }
}

enum Request {
  #[cfg(unix)]
  Master,
  Configure(bool),
  List,
}

async fn serve_forwarding(
  listener: Listener,
  expected_target: ctl_ipc::SshTarget,
  status: PortForwardStatus,
) {
  // Windows must configure its dedicated SSH process without asking for a
  // control master. Unix retains the established authentication boundary.
  for expected in [
    #[cfg(unix)]
    Request::Master,
    Request::Configure(true),
    Request::List,
    Request::List,
    #[cfg(unix)]
    Request::Master,
    Request::Configure(true),
    Request::List,
    Request::Configure(false),
  ] {
    let mut stream = listener.accept().await.unwrap().0;
    assert!(matches!(
      ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
        .await
        .unwrap(),
      Some(ClientMessage::Handshake { .. }),
    ));
    ctl_ipc::write_frame(
      &mut stream,
      &ServerMessage::HandshakeAccepted {
        protocol_version: ctl_ipc::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    let request = ctl_ipc::read_frame(&mut stream).await.unwrap();
    let response = match (expected, request) {
      #[cfg(unix)]
      (Request::Master, Some(ClientMessage::EnsureMaster { target })) => {
        assert_eq!(target, expected_target);
        ServerMessage::MasterReady {
          control_path: PathBuf::from("unused-master"),
        }
      }
      (Request::List, Some(ClientMessage::ListPortForwards { target })) => {
        assert_eq!(target, expected_target);
        ServerMessage::PortForwards {
          statuses: vec![status.clone()],
        }
      }
      (
        Request::Configure(expected_enabled),
        Some(ClientMessage::ConfigurePortForward {
          target,
          forward,
          enabled,
        }),
      ) => {
        assert_eq!(target, expected_target);
        assert_eq!(forward, status.forward);
        assert_eq!(enabled, expected_enabled);
        ServerMessage::PortForwardConfigured {
          status: status.clone(),
        }
      }
      _ => panic!("unexpected forwarding broker operation"),
    };
    ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
  }
}

#[tokio::test]
async fn forwards_preserve_saved_and_temporary_targets_through_native_ipc() {
  for saved in [true, false] {
    let fixture = Fixture::new();
    let listener = Listener::bind(&fixture.socket_path).unwrap();
    let target: ConnectionTargetDto = serde_json::from_value(if saved {
      json!({
        "kind": "ssh", "destination": "configured", "ssh_config_alias": "configured",
        "hostname": "192.0.2.20", "user": "saved-user", "port": 2222,
        "identity_file": "identity with spaces"
      })
    } else {
      json!({ "kind": "ssh", "destination": "configured", "ssh_config_alias": "configured" })
    })
    .unwrap();
    let expected_target = target.to_ssh_target().unwrap();
    let status = PortForwardStatus {
      forward: LocalPortForward {
        forward_id: "web".into(),
        bind_address: "127.0.0.1".into(),
        local_port: 8080,
        remote_host: "localhost".into(),
        remote_port: 80,
      },
      state: PortForwardState::Active,
      message: None,
    };
    let expected_status = status.clone();
    let peer = tokio::spawn(serve_forwarding(listener, expected_target, status));
    for arguments in [
      &["add", "8080:localhost:80", "--id", "web", "--json"][..],
      &["list", "--json"],
      &["add", "8080:localhost:80", "--json"],
    ] {
      let output = fixture.run(saved, arguments).await;
      assert_eq!(
        serde_json::from_slice::<Vec<PortForwardStatus>>(&output.stdout).unwrap(),
        std::slice::from_ref(&expected_status),
      );
    }
    fixture.run(saved, &["remove", "web"]).await;
    peer.await.unwrap();
  }
}
