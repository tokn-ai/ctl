use super::*;
use ctld_ipc::{ClientMessage, ServerMessage, SshGatewayMode, VpnSnapshot, VpnStatus};
use std::path::PathBuf;
use tokio::net::{TcpListener, UnixListener};
use tokio::task::JoinHandle;

struct OwnerFixture {
  path: PathBuf,
  task: JoinHandle<()>,
}

impl OwnerFixture {
  fn new(snapshots: Vec<Vec<VpnStatus>>) -> Self {
    // Keep the Unix socket short enough for macOS regardless of TMPDIR.
    let path = PathBuf::from(format!("/tmp/ctld-vpn-route-{}.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&path).unwrap();
    let task = tokio::spawn(async move {
      for connections in snapshots {
        let (mut stream, _) = listener.accept().await.unwrap();
        assert!(matches!(
          ctld_ipc::read_frame(&mut stream).await.unwrap(),
          Some(ClientMessage::Handshake { .. })
        ));
        ctld_ipc::write_frame(
          &mut stream,
          &ServerMessage::HandshakeAccepted {
            protocol_version: ctld_ipc::PROTOCOL_VERSION,
          },
        )
        .await
        .unwrap();
        assert!(matches!(
          ctld_ipc::read_frame(&mut stream).await.unwrap(),
          Some(ClientMessage::VpnStatus)
        ));
        ctld_ipc::write_frame(
          &mut stream,
          &ServerMessage::VpnStatus {
            status: connections.first().cloned().unwrap_or_default().into(),
            snapshot: Some(VpnSnapshot {
              connections,
              supports_multiple: true,
              ..VpnSnapshot::default()
            }),
          },
        )
        .await
        .unwrap();
      }
    });
    Self { path, task }
  }

  fn gateway(&self) -> SshGateway {
    SshGateway {
      kind: GatewayKind::Vpn,
      vpn: Some(VpnGateway {
        connection_id: "saved-vpn".into(),
        socket_path: self.path.clone(),
      }),
      destination: "saved-vpn".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    }
  }
}

impl Drop for OwnerFixture {
  fn drop(&mut self) {
    self.task.abort();
    let _ = std::fs::remove_file(&self.path);
  }
}

fn connected(endpoint: &str) -> VpnStatus {
  VpnStatus {
    vpn_id: Some("saved-vpn".into()),
    connection_id: Some("saved-vpn".into()),
    state: VpnState::Connected,
    running: true,
    endpoint: Some(endpoint.into()),
    ..VpnStatus::default()
  }
}

async fn socks_fixture(marker: u8) -> (String, JoinHandle<()>) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let endpoint = format!("socks5h://{}", listener.local_addr().unwrap());
  let task = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut greeting = [0; 3];
    stream.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 1, 0]);
    stream.write_all(&[5, 0]).await.unwrap();
    let mut request = [0; 5];
    stream.read_exact(&mut request).await.unwrap();
    assert_eq!(request, [5, 1, 0, 3, 15]);
    let mut rest = [0; 17];
    stream.read_exact(&mut rest).await.unwrap();
    assert_eq!(&rest[..15], b"target.internal");
    assert_eq!(u16::from_be_bytes([rest[15], rest[16]]), 2222);
    stream
      .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0, marker])
      .await
      .unwrap();
  });
  (endpoint, task)
}

#[tokio::test]
async fn each_stream_resolves_the_current_vpn_endpoint_without_changing_route_identity() {
  let (first_endpoint, first_server) = socks_fixture(1).await;
  let (second_endpoint, second_server) = socks_fixture(2).await;
  let owner = OwnerFixture::new(vec![
    vec![connected(&first_endpoint)],
    vec![connected(&second_endpoint)],
  ]);
  let route = vec![owner.gateway()];
  let identity = serde_json::to_vec(&route).unwrap();
  for marker in [1, 2] {
    let mut stream = connect(&route, "target.internal", 2222).await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), marker);
    assert_eq!(serde_json::to_vec(&route).unwrap(), identity);
  }
  first_server.await.unwrap();
  second_server.await.unwrap();
}

#[tokio::test]
async fn unavailable_or_unready_vpns_never_fall_back_to_the_destination() {
  let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let mut wrong_connection = connected("socks5h://127.0.0.1:1080");
  wrong_connection.connection_id = Some("other-vpn".into());
  let mut starting = connected("socks5h://127.0.0.1:1080");
  starting.state = VpnState::Starting;
  let mut stopped = connected("socks5h://127.0.0.1:1080");
  stopped.running = false;
  let duplicate = connected("socks5h://127.0.0.1:1080");
  let owner = OwnerFixture::new(vec![
    vec![],
    vec![wrong_connection],
    vec![starting],
    vec![stopped],
    vec![duplicate.clone(), duplicate],
    vec![connected("socks5h://192.0.2.1:1080")],
  ]);
  let route = [owner.gateway()];
  for _ in 0..6 {
    tokio::select! {
      accepted = destination.accept() => {
        drop(accepted);
        panic!("VPN failure contacted the destination directly");
      }
      result = connect(&route, "127.0.0.1", destination.local_addr().unwrap().port()) => {
        assert!(result.is_err());
      }
    }
  }
}

#[test]
fn vpn_endpoints_require_a_numeric_loopback_socks5h_address() {
  for valid in ["socks5h://127.0.0.1:1080", "socks5h://[::1]:1080"] {
    assert!(parse_vpn_endpoint(valid).is_ok());
  }
  for invalid in [
    "socks5://127.0.0.1:1080",
    "socks5h://localhost:1080",
    "socks5h://192.0.2.1:1080",
    "socks5h://[2001:db8::1]:1080",
    "socks5h://127.0.0.1:0",
    "socks5h://127.0.0.1",
    "socks5h://user@127.0.0.1:1080",
    "socks5h://@127.0.0.1:1080",
    "socks5h://127.0.0.1:1080/",
    "socks5h://127.0.0.1:1080?ignored",
    "socks5h://127.0.0.1:1080#ignored",
  ] {
    assert!(parse_vpn_endpoint(invalid).is_err(), "{invalid}");
  }
}

#[tokio::test]
async fn a_vpn_after_another_hop_is_rejected_before_contacting_its_owner() {
  let owner = OwnerFixture::new(vec![]);
  let gateway = owner.gateway();
  let Err(error) = connect(&[gateway.clone(), gateway], "target.internal", 2222).await else {
    panic!("remote VPN hop must be rejected");
  };
  assert!(error.to_string().contains("first, local gateway"));
}

#[tokio::test]
async fn direct_connections_do_not_depend_on_a_vpn_owner() {
  let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let mut stream = connect(&[], "127.0.0.1", destination.local_addr().unwrap().port())
    .await
    .unwrap();
  let (mut accepted, _) = destination.accept().await.unwrap();
  accepted.write_all(b"direct").await.unwrap();
  let mut response = [0; 6];
  stream.read_exact(&mut response).await.unwrap();
  assert_eq!(&response, b"direct");
}
