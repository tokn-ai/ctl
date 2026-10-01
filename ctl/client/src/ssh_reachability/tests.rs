use super::*;
use ctl_ipc::{SshGatewayMode, VpnSnapshot, VpnStatus};
use tokio::net::TcpListener;

fn endpoint(address: SocketAddr) -> config::Endpoint {
  config::Endpoint {
    host: address.ip().to_string(),
    port: address.port(),
  }
}

fn socks_gateway(address: SocketAddr) -> SshGateway {
  SshGateway {
    kind: GatewayKind::Socks5,
    vpn: None,
    destination: address.ip().to_string(),
    hostname: None,
    user: None,
    port: Some(address.port()),
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  }
}

async fn check(gateways: &[SshGateway], endpoint: &config::Endpoint) -> SshReachability {
  let checking_service = AtomicBool::new(false);
  if let Err(result) = preflight_route(gateways) {
    return result;
  }
  within_deadline(
    Duration::from_secs(2),
    &checking_service,
    probe_endpoint(gateways, endpoint, &checking_service),
  )
  .await
}

#[tokio::test]
async fn direct_greeting_closes_without_sending_identification_or_authentication() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    stream
      .write_all(b"Welcome\r\nSSH-2.0-OpenSSH_9.9 test fixture\r\n")
      .await
      .unwrap();
    let mut received = Vec::new();
    stream.read_to_end(&mut received).await.unwrap();
    assert!(received.is_empty(), "passive check sent SSH payload");
  });
  assert_eq!(
    check(&[], &endpoint(address)).await,
    SshReachability::available()
  );
  server.await.unwrap();
}

#[tokio::test]
async fn greeting_requires_supported_complete_identification_and_bounds_preamble() {
  for bytes in [
    b"SSH-2.0-OpenSSH_9.9\r\n".as_slice(),
    b"notice\nSSH-1.99-compatible\n",
    "欢迎\r\nSSH-2.0-fixture\r\n".as_bytes(),
  ] {
    assert!(read_greeting(&mut &*bytes).await.is_ok());
  }
  let too_long = format!("SSH-2.0-{}\r\n", "x".repeat(255));
  for bytes in [
    b"HTTP/1.1 200 OK\r\n".as_slice(),
    b"SSH-1.5-old\r\n",
    b"SSH-2.0-\r\n",
    b"SSH-2.0-software-with-hyphen\r\n",
    b"SSH-2.0-server\0\r\n",
    b"SSH-2.0-unfinished",
    too_long.as_bytes(),
    &vec![b'x'; MAX_GREETING_BYTES + 1],
  ] {
    let result = read_greeting(&mut &*bytes).await.unwrap_err();
    assert_eq!(result.reason, Some(SshReachabilityReason::InvalidGreeting));
  }
}

#[tokio::test]
async fn a_non_ssh_listener_is_unavailable() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
  });
  let result = check(&[], &endpoint(address)).await;
  assert_eq!(result.state, SshReachabilityState::Unavailable);
  assert_eq!(result.reason, Some(SshReachabilityReason::InvalidGreeting));
  server.await.unwrap();
}

#[test]
fn refused_service_is_unavailable_but_refused_proxy_is_unknown() {
  let result = network_error(io::ErrorKind::ConnectionRefused.into());
  assert_eq!(result.state, SshReachabilityState::Unavailable);
  assert_eq!(
    result.reason,
    Some(SshReachabilityReason::ConnectionRefused)
  );
  let result = proxy_error(io::ErrorKind::ConnectionRefused.into());
  assert_eq!(result.state, SshReachabilityState::Unknown);
}

#[tokio::test]
async fn closed_service_is_unavailable_but_closed_proxy_is_unknown() {
  // Reserve the port without listening so another test cannot claim it.
  let socket = tokio::net::TcpSocket::new_v4().unwrap();
  socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
  let address = socket.local_addr().unwrap();
  let result = check(&[], &endpoint(address)).await;
  assert_eq!(result.state, SshReachabilityState::Unavailable);
  // Some platforms report refusal only after our bounded check expires.
  assert!(matches!(
    result.reason,
    Some(SshReachabilityReason::ConnectionRefused | SshReachabilityReason::TimedOut)
  ));
  let result = check(&[socks_gateway(address)], &endpoint(address)).await;
  assert_eq!(result.state, SshReachabilityState::Unknown);
}

#[tokio::test]
async fn timeout_before_target_probe_is_unknown_and_greeting_timeout_closes_socket() {
  let phase = AtomicBool::new(false);
  let result = within_deadline(Duration::from_millis(10), &phase, std::future::pending()).await;
  assert_eq!(result.state, SshReachabilityState::Unknown);
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
  });
  let phase = AtomicBool::new(false);
  let result = within_deadline(
    Duration::from_millis(100),
    &phase,
    probe_endpoint(&[], &endpoint(address), &phase),
  )
  .await;
  assert_eq!(result.state, SshReachabilityState::Unavailable);
  assert_eq!(result.reason, Some(SshReachabilityReason::TimedOut));
  server.await.unwrap();
}

#[tokio::test]
async fn preflights_every_hop_before_opening_the_first_proxy() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let first = socks_gateway(address);
  let mut unsupported = first.clone();
  unsupported.kind = GatewayKind::Ssh;
  let result = check(&[first.clone(), unsupported], &endpoint(address)).await;
  assert_eq!(
    result.reason,
    Some(SshReachabilityReason::RouteRequiresConnection)
  );
  let mut authenticated = first.clone();
  authenticated.user = Some("fixture".into());
  let result = check(&[first, authenticated], &endpoint(address)).await;
  assert_eq!(
    result.reason,
    Some(SshReachabilityReason::RouteRequiresConnection)
  );
  assert!(
    tokio::time::timeout(Duration::from_millis(10), listener.accept())
      .await
      .is_err()
  );
}

#[test]
fn no_auth_socks_allows_default_port_but_invalid_routes_are_skipped() {
  let mut gateway = socks_gateway("127.0.0.1:1080".parse().unwrap());
  gateway.port = None;
  assert!(preflight_route(&[gateway.clone()]).is_ok());
  gateway.port = Some(0);
  assert!(preflight_route(&[gateway.clone()]).is_err());
  gateway.port = Some(1080);
  assert!(preflight_route(&vec![gateway; MAX_ROUTE_HOPS + 1]).is_err());
}

async fn accept_socks(stream: &mut TcpStream, expected_host: &str, expected_port: u16) {
  let mut negotiation = [0; 3];
  stream.read_exact(&mut negotiation).await.unwrap();
  assert_eq!(negotiation, [5, 1, 0]);
  stream.write_all(&[5, 0]).await.unwrap();
  let mut header = [0; 4];
  stream.read_exact(&mut header).await.unwrap();
  assert_eq!(header, [5, 1, 0, 3]);
  let length = stream.read_u8().await.unwrap();
  let mut host = vec![0; usize::from(length)];
  stream.read_exact(&mut host).await.unwrap();
  assert_eq!(host, expected_host.as_bytes());
  assert_eq!(stream.read_u16().await.unwrap(), expected_port);
  stream
    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
    .await
    .unwrap();
}

#[tokio::test]
async fn socks_hops_preserve_order_and_resolve_names_remotely() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    accept_socks(&mut stream, "second.invalid", 1081).await;
    accept_socks(&mut stream, "target.invalid", 2222).await;
    stream.write_all(b"SSH-2.0-fixture\r\n").await.unwrap();
    let mut received = Vec::new();
    stream.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, Vec::<u8>::new());
  });
  let mut second = socks_gateway(address);
  second.destination = "second.invalid".into();
  second.port = Some(1081);
  let endpoint = config::Endpoint {
    host: "target.invalid".into(),
    port: 2222,
  };
  assert_eq!(
    check(&[socks_gateway(address), second], &endpoint).await,
    SshReachability::available()
  );
  server.await.unwrap();
}

#[tokio::test]
async fn socks_authentication_request_is_declined_without_credentials() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let server = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut negotiation = [0; 3];
    stream.read_exact(&mut negotiation).await.unwrap();
    assert_eq!(negotiation, [5, 1, 0]);
    stream.write_all(&[5, 2]).await.unwrap();
    let mut received = Vec::new();
    stream.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, Vec::<u8>::new());
  });
  let result = check(&[socks_gateway(address)], &endpoint(address)).await;
  assert_eq!(
    result.reason,
    Some(SshReachabilityReason::RouteRequiresConnection)
  );
  server.await.unwrap();
}

fn connected_vpn(endpoint: &str) -> VpnStatus {
  VpnStatus {
    connection_id: Some("fixture-vpn".into()),
    running: true,
    state: VpnState::Connected,
    endpoint: Some(endpoint.into()),
    ..VpnStatus::default()
  }
}

#[test]
fn vpn_selection_requires_exact_unique_active_owner_and_loopback_endpoint() {
  let status = connected_vpn("socks5h://127.0.0.1:1080");
  let snapshot = |connections| VpnSnapshot {
    connections,
    ..VpnSnapshot::default()
  };
  assert_eq!(
    connected_vpn_endpoint(&snapshot(vec![status.clone()]), "fixture-vpn").unwrap(),
    "127.0.0.1:1080".parse::<SocketAddr>().unwrap()
  );
  let mut stopped = status.clone();
  stopped.state = VpnState::Stopped;
  let mut starting = status.clone();
  starting.state = VpnState::Starting;
  let mut not_running = status.clone();
  not_running.running = false;
  let mut other = status.clone();
  other.connection_id = Some("other-vpn".into());
  for statuses in [
    vec![],
    vec![stopped],
    vec![starting],
    vec![not_running],
    vec![other],
  ] {
    assert_eq!(
      connected_vpn_endpoint(&snapshot(statuses), "fixture-vpn")
        .unwrap_err()
        .reason,
      Some(SshReachabilityReason::VpnDisconnected)
    );
  }
  assert_eq!(
    connected_vpn_endpoint(&snapshot(vec![status.clone(), status]), "fixture-vpn")
      .unwrap_err()
      .state,
    SshReachabilityState::Unknown
  );
  for endpoint in [
    "socks5://127.0.0.1:1080",
    "socks5h://localhost:1080",
    "socks5h://192.0.2.1:1080",
    "socks5h://127.0.0.1:0",
    "socks5h://127.0.0.1:1080/",
    "socks5h://user@127.0.0.1:1080",
  ] {
    assert!(
      connected_vpn_endpoint(&snapshot(vec![connected_vpn(endpoint)]), "fixture-vpn").is_err()
    );
  }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_vpn_uses_only_status_request_and_existing_socks_listener() {
  use ctl_ipc::{ClientMessage, ServerMessage};
  let path = std::path::PathBuf::from(format!("/tmp/ctl-probe-{}.sock", uuid::Uuid::new_v4()));
  let owner = tokio::net::UnixListener::bind(&path).unwrap();
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let owner_task = tokio::spawn(async move {
    let (mut stream, _) = owner.accept().await.unwrap();
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
    assert!(matches!(
      ctl_ipc::read_frame(&mut stream).await.unwrap(),
      Some(ClientMessage::VpnStatus)
    ));
    let status = connected_vpn(&format!("socks5h://{address}"));
    ctl_ipc::write_frame(
      &mut stream,
      &ServerMessage::VpnStatus {
        status: status.clone().into(),
        snapshot: Some(VpnSnapshot {
          connections: vec![status],
          ..VpnSnapshot::default()
        }),
      },
    )
    .await
    .unwrap();
  });
  let socks = tokio::spawn(async move {
    let (mut stream, _) = listener.accept().await.unwrap();
    accept_socks(&mut stream, "target.invalid", 2222).await;
    stream.write_all(b"SSH-2.0-fixture\r\n").await.unwrap();
  });
  let gateway = SshGateway {
    kind: GatewayKind::Vpn,
    destination: "fixture-vpn".into(),
    vpn: Some(VpnGateway {
      connection_id: "fixture-vpn".into(),
      socket_path: path.clone(),
    }),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  };
  let endpoint = config::Endpoint {
    host: "target.invalid".into(),
    port: 2222,
  };
  let result = check(&[gateway], &endpoint).await;
  owner_task.await.unwrap();
  socks.await.unwrap();
  std::fs::remove_file(path).unwrap();
  assert_eq!(result, SshReachability::available());
}

#[cfg(unix)]
#[tokio::test]
async fn missing_vpn_owner_is_not_started_and_cannot_fall_back_direct() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  let path = std::path::PathBuf::from(format!(
    "/tmp/ctl-probe-missing-{}.sock",
    uuid::Uuid::new_v4()
  ));
  let gateway = SshGateway {
    kind: GatewayKind::Vpn,
    destination: "fixture-vpn".into(),
    vpn: Some(VpnGateway {
      connection_id: "fixture-vpn".into(),
      socket_path: path.clone(),
    }),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  };
  let result = check(&[gateway], &endpoint(address)).await;
  assert_eq!(result.reason, Some(SshReachabilityReason::VpnDisconnected));
  assert!(!path.exists());
  assert!(
    tokio::time::timeout(Duration::from_millis(10), listener.accept())
      .await
      .is_err()
  );
}
