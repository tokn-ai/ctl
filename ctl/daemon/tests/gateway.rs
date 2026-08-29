#![cfg(unix)]

use ctl_core::{ClientIdentity, CoreError, HostConfig, open_rmux_tunnel, pair};
use ctl_proto::ErrorCode;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct TestPaths {
  state_dir: PathBuf,
  rmux_socket: PathBuf,
}

impl TestPaths {
  fn new() -> Self {
    let token = Uuid::new_v4().simple().to_string();
    Self {
      state_dir: PathBuf::from(format!("/tmp/ctld-state-{}", &token[..12])),
      rmux_socket: PathBuf::from(format!("/tmp/ctld-rmux-{}.sock", &token[..12])),
    }
  }
}

impl Drop for TestPaths {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.rmux_socket);
    let _ = std::fs::remove_dir_all(&self.state_dir);
  }
}

fn host_from_invitation(invitation: &ctl_proto::PairingInvitation) -> HostConfig {
  HostConfig {
    alias: "test-host".into(),
    endpoint: invitation.endpoint.clone(),
    server_name: invitation.server_name.clone(),
    device_id: invitation.device_id.clone(),
    device_certificate_base64: invitation.device_certificate_base64.clone(),
  }
}

fn expiration_after(duration: Duration) -> u64 {
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("system clock is after Unix epoch");
  u64::try_from((now + duration).as_millis()).expect("test timestamp fits u64")
}

fn spawn_fake_rmux_endpoint(
  socket_path: &std::path::Path,
  relay_payload: &'static [u8],
) -> JoinHandle<()> {
  let rmux_listener = UnixListener::bind(socket_path).expect("bind fake rmux endpoint");
  tokio::spawn(async move {
    let (mut first, _) = rmux_listener
      .accept()
      .await
      .expect("first gateway connection");
    let mut received = vec![0; relay_payload.len()];
    first
      .read_exact(&mut received)
      .await
      .expect("read gateway payload");
    assert_eq!(received, relay_payload);
    first
      .write_all(&received)
      .await
      .expect("echo gateway payload");
    first.flush().await.expect("flush gateway echo");

    let mut probe = [0_u8; 1];
    let closed = timeout(TEST_TIMEOUT, first.read(&mut probe))
      .await
      .expect("ctld shutdown closes the first relay")
      .expect("first relay reads cleanly after ctld shutdown");
    assert_eq!(
      closed, 0,
      "ctld left the local relay attached after shutdown"
    );
    drop(first);

    // `ctld` is gone at this point. The endpoint must still represent the
    // same local service and accept a direct attachment.
    let (mut direct, _) = rmux_listener
      .accept()
      .await
      .expect("direct local connection");
    let mut request = [0_u8; 4];
    direct
      .read_exact(&mut request)
      .await
      .expect("read direct endpoint request");
    assert_eq!(&request, b"ping");
    direct
      .write_all(b"pong")
      .await
      .expect("write direct endpoint response");
    direct
      .flush()
      .await
      .expect("flush direct endpoint response");
  })
}

async fn assert_unpaired_identity_cannot_open_service(invitation: &ctl_proto::PairingInvitation) {
  let unpaired_identity = ClientIdentity::generate().expect("generate unpaired identity");
  let unpaired_error = open_rmux_tunnel(
    &host_from_invitation(invitation),
    &unpaired_identity,
    "ctld-gateway-test",
    "0.1.0",
  )
  .await
  .expect_err("unpaired identity must not open rmux");
  assert!(matches!(
    unpaired_error,
    CoreError::Server {
      code: ErrorCode::AuthenticationFailed,
      ..
    }
  ));
}

async fn assert_local_endpoint_survives_shutdown(socket_path: &std::path::Path) {
  let mut direct = UnixStream::connect(socket_path)
    .await
    .expect("local endpoint survives ctld shutdown");
  direct
    .write_all(b"ping")
    .await
    .expect("write direct local request");
  direct.flush().await.expect("flush direct local request");
  let mut response = [0_u8; 4];
  direct
    .read_exact(&mut response)
    .await
    .expect("read direct local response");
  assert_eq!(&response, b"pong");
}

/// Verifies the complete wire boundary: a TLS-pinned client pairs, a client
/// that never paired cannot select a service, and the one-way control upgrade
/// preserves raw bytes in both directions. The local endpoint is a small
/// echo server rather than `rmuxd`, so the test precisely observes that ctld
/// neither parses nor owns the inner service stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_pairs_authenticates_and_relays_without_owning_the_local_endpoint() {
  let paths = TestPaths::new();
  let state = ctld::initialize(&paths.state_dir).expect("initialize device state");
  let listener = TcpListener::bind("127.0.0.1:0")
    .await
    .expect("bind ctld listener");
  let endpoint = listener.local_addr().expect("ctld listener address");
  let invitation = ctld::create_pairing_invitation(
    &paths.state_dir,
    endpoint.to_string(),
    "integration-client".into(),
    expiration_after(Duration::from_mins(1)),
  )
  .expect("create pairing invitation");
  assert_eq!(invitation.device_id, state.device_id);

  let relay_payload = b"\0raw rmux payload\xff\n";
  let endpoint_task = spawn_fake_rmux_endpoint(&paths.rmux_socket, relay_payload);

  let config =
    ctld::DaemonConfig::with_defaults(paths.state_dir.clone(), paths.rmux_socket.clone());
  let (shutdown_tx, shutdown_rx) = watch::channel(false);
  let daemon_task = tokio::spawn(async move { ctld::serve(listener, config, shutdown_rx).await });

  assert_unpaired_identity_cannot_open_service(&invitation).await;

  let paired_identity = ClientIdentity::generate().expect("generate paired identity");
  let host = pair(
    &invitation,
    "test-host".into(),
    &paired_identity,
    "ctld-gateway-test",
    "0.1.0",
  )
  .await
  .expect("pair over the invitation's pinned TLS certificate");

  // `open_rmux_tunnel` returns only after `ServiceOpened` is received. An
  // immediate first write exercises the control-to-raw upgrade boundary.
  let mut tunnel = open_rmux_tunnel(&host, &paired_identity, "ctld-gateway-test", "0.1.0")
    .await
    .expect("paired identity opens rmux tunnel");
  tunnel
    .write_all(relay_payload)
    .await
    .expect("write first raw bytes immediately after service upgrade");
  tunnel.flush().await.expect("flush raw bytes");
  let mut echoed = vec![0; relay_payload.len()];
  tunnel
    .read_exact(&mut echoed)
    .await
    .expect("read raw bytes relayed from endpoint");
  assert_eq!(echoed, relay_payload);

  shutdown_tx.send(true).expect("signal ctld shutdown");
  daemon_task
    .await
    .expect("join ctld task")
    .expect("ctld shuts down cleanly");

  let mut close_probe = [0_u8; 1];
  match timeout(TEST_TIMEOUT, tunnel.read(&mut close_probe))
    .await
    .expect("ctld shutdown resolves tunnel read")
  {
    Ok(0) | Err(_) => {}
    Ok(count) => panic!("expected closed tunnel after ctld shutdown, received {count} bytes"),
  }
  drop(tunnel);

  assert_local_endpoint_survives_shutdown(&paths.rmux_socket).await;

  endpoint_task.await.expect("join fake rmux endpoint");
}
