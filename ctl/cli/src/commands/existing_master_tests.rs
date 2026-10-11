use super::*;
use ctl_ipc::{ClientMessage, ServerMessage};
use std::os::unix::fs::DirBuilderExt as _;
use std::time::Duration;
use tokio::net::UnixListener;

const CHILD_TEST: &str = "commands::existing_master_tests::existing_master_fixture";
const CONTROL_PATH: &str = "/fixture/existing-target-master";
const VPN_SOCKET: &str = "/fixture/vpn-must-not-start";

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    // macOS's per-user temporary root leaves little space in sockaddr_un.
    let directory =
      std::env::temp_dir().join(format!("cem-{:016x}", uuid::Uuid::new_v4().as_u64_pair().1));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .unwrap();
    Self(directory)
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn settings() -> ctl_client::hosts::ConnectionTargetDto {
  let mut target = ctl_client::hosts::ConnectionTargetDto::ssh("selected-host");
  if let ctl_client::hosts::ConnectionTargetDto::Ssh {
    hostname,
    user,
    port,
    vpn_connection_id,
    ..
  } = &mut target
  {
    *hostname = Some("selected.example.invalid".into());
    *user = Some("selected-account".into());
    *port = Some(2222);
    *vpn_connection_id = Some("vpn-must-not-start".into());
  }
  target
}

async fn run_fixture(mode: &str) {
  let directory = Fixture::new();
  let socket = directory.0.join("s");
  let listener = UnixListener::bind(&socket).unwrap();
  let child = tokio::process::Command::new(std::env::current_exe().unwrap())
    .args(["--ignored", "--exact", CHILD_TEST, "--nocapture"])
    .env_clear()
    .env("HOME", &directory.0)
    .env("PATH", "/usr/bin:/bin")
    .env("CTLD_SOCKET_PATH", &socket)
    .env("CTLD_VPN_SOCKET_PATH", VPN_SOCKET)
    .env("CTL_EXISTING_MASTER_TEST_MODE", mode)
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
    .await
    .expect("fixture did not connect to its private broker")
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
      .await
      .unwrap(),
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
  let mut expected = settings().to_ssh_target().unwrap();
  expected.gateways[0].vpn.as_mut().unwrap().socket_path = VPN_SOCKET.into();
  for _ in 0..if mode == "ready" { 2 } else { 1 } {
    assert!(matches!(
      ctl_ipc::read_frame::<_, ClientMessage>(&mut stream).await.unwrap(),
      Some(ClientMessage::MasterStatus { target }) if target == expected
    ));
    let response = if mode == "ready" {
      ServerMessage::MasterReady {
        control_path: CONTROL_PATH.into(),
      }
    } else {
      ServerMessage::Error {
        code: mode.into(),
        message: "requires explicit user action".into(),
      }
    };
    ctl_ipc::write_frame(&mut stream, &response).await.unwrap();
  }
  let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
    .await
    .expect("fixture must not wait for SSH, VPN startup, or authentication")
    .unwrap();
  assert!(
    output.status.success(),
    "fixture failed:\n{}\n{}",
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr),
  );
  assert!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
      .await
      .unwrap()
      .is_none(),
    "passive reuse must not issue an EnsureMaster or VPN operation",
  );
}

#[tokio::test]
async fn existing_master_bypasses_vpn_preparation_for_quiet_and_interactive_channels() {
  run_fixture("ready").await;
}

#[tokio::test]
async fn passive_reuse_preserves_manual_disconnect_and_unknown_observations() {
  for mode in ["ssh_host_disconnected", "ssh_status_unknown"] {
    run_fixture(mode).await;
  }
}

#[tokio::test]
#[ignore = "private subprocess fixture"]
async fn existing_master_fixture() {
  let mode = std::env::var("CTL_EXISTING_MASTER_TEST_MODE").unwrap();
  let broker = crate::ssh_broker::MasterClient::default();
  if mode == "ready" {
    for interactive in [false, true] {
      assert!(matches!(
        ssh_interaction(&settings(), &broker, interactive).await.unwrap(),
        ctl_client::SshInteraction::Multiplexed { control_path }
          if control_path == std::path::Path::new(CONTROL_PATH)
      ));
    }
  } else {
    assert!(matches!(
      ssh_interaction(&settings(), &broker, false).await,
      Err(CtlConnectError::Broker(crate::ssh_broker::Error::Daemon { code, .. }))
        if code == mode
    ));
  }
}
