use super::*;
use ctl_ipc::VpnStatus;
#[cfg(unix)]
use ctl_ipc::{ClientMessage, ServerMessage};

fn connected(endpoint: &str) -> VpnStatus {
  VpnStatus {
    connection_id: Some("work".into()),
    running: true,
    state: VpnState::Connected,
    endpoint: Some(endpoint.into()),
    ..VpnStatus::default()
  }
}

#[cfg(unix)]
fn identity() -> ctl_proto::RemoteIdentity {
  ctl_proto::RemoteIdentity {
    remote_id: uuid::Uuid::new_v4().to_string(),
    agent_version: "0.1.0".into(),
    build: None,
    ctmux_restart_supported: false,
    bundle: None,
    protocols: crate::agent_protocols(),
  }
}

#[test]
fn only_exact_unique_connected_loopback_endpoints_are_accepted() {
  let snapshot = |connections| VpnSnapshot {
    connections,
    ..VpnSnapshot::default()
  };
  assert_eq!(
    connected_endpoint(
      &snapshot(vec![connected("socks5h://127.0.0.1:1080")]),
      "work"
    )
    .unwrap(),
    "127.0.0.1:1080".parse::<SocketAddr>().unwrap()
  );
  for endpoint in [
    "socks5h://example.test:1080",
    "socks5h://192.168.1.2:1080",
    "socks5h://127.0.0.1:0",
    "socks5h://user:secret@127.0.0.1:1080",
    "socks5h://127.0.0.1:1080/path",
  ] {
    assert!(connected_endpoint(&snapshot(vec![connected(endpoint)]), "work").is_err());
  }
  let status = connected("socks5h://127.0.0.1:1080");
  assert!(connected_endpoint(&snapshot(vec![status.clone(), status.clone()]), "work").is_err());
  assert!(connected_endpoint(&snapshot(vec![status.clone()]), "other").is_err());
  let mut stopped = status;
  stopped.status_unavailable = true;
  assert!(connected_endpoint(&snapshot(vec![stopped.clone()]), "work").is_err());
  stopped.status_unavailable = false;
  stopped.state = VpnState::Stopping;
  assert!(connected_endpoint(&snapshot(vec![stopped]), "work").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn unsupported_contracts_never_send_identity_or_open_the_vpn_daemon() {
  let socket =
    std::env::temp_dir().join(format!("ctl-vpn-negotiation-{}.sock", uuid::Uuid::new_v4()));
  let client =
    ctl_ipc::vpn::Client::new(socket.clone()).with_daemon_executable("/does/not/exist/ctld".into());
  for selection in [
    serde_json::json!({ "protocol_version": "1.1.3" }),
    serde_json::json!({ "protocol_version": "2.0.3" }),
    serde_json::json!({ "protocol_version": "1.0.1", "unexpected": true }),
  ] {
    let mut input = Vec::new();
    ctl_ipc::write_frame(&mut input, &selection).await.unwrap();
    let mut output = Vec::new();
    assert!(
      serve(&mut input.as_slice(), &mut output, &client, &identity())
        .await
        .is_err()
    );
    assert_eq!(&output[..PREFACE.len()], PREFACE);
    let mut remainder = &output[PREFACE.len()..];
    let offer: ctl_core::protocol::ProtocolOffer =
      ctl_ipc::read_frame(&mut remainder).await.unwrap().unwrap();
    assert_eq!(offer, ctl_ipc::remote_vpn::protocol_offer());
    assert_eq!(remainder, [] as [u8; 0]);
    assert!(!socket.exists());
  }
}

#[cfg(unix)]
fn negotiation_channel() -> (
  tokio::io::DuplexStream,
  tokio::task::JoinHandle<io::Result<()>>,
) {
  let socket = std::env::temp_dir().join(format!("cvn-{}.s", uuid::Uuid::new_v4().simple()));
  let client =
    ctl_ipc::vpn::Client::new(socket).with_daemon_executable("/does/not/exist/ctld".into());
  let identity = identity();
  let (channel, gateway) = tokio::io::duplex(4096);
  let server = tokio::spawn(async move {
    let (mut reader, mut writer) = tokio::io::split(gateway);
    serve(&mut reader, &mut writer, &client, &identity).await
  });
  (channel, server)
}

#[cfg(unix)]
async fn read_offer(reader: &mut (impl AsyncRead + Unpin)) {
  let mut preface = [0; PREFACE.len()];
  reader.read_exact(&mut preface).await.unwrap();
  assert_eq!(preface, PREFACE);
  let offer: ctl_core::protocol::ProtocolOffer =
    ctl_ipc::read_frame(reader).await.unwrap().unwrap();
  assert_eq!(offer, ctl_ipc::remote_vpn::protocol_offer());
}

#[cfg(unix)]
async fn select_contract(writer: &mut (impl AsyncWrite + Unpin)) {
  ctl_ipc::write_frame(
    writer,
    &ctl_ipc::remote_vpn::ProtocolSelection {
      protocol_version: ctl_ipc::remote_vpn::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn delayed_selection_and_request_use_the_supported_startup_budget() {
  let (mut channel, server) = negotiation_channel();
  read_offer(&mut channel).await;
  tokio::time::advance(Duration::from_secs(20)).await;
  tokio::task::yield_now().await;
  assert!(!server.is_finished());
  select_contract(&mut channel).await;
  assert!(
    ctl_proto::read_identity(&mut channel)
      .await
      .unwrap()
      .is_valid()
  );
  tokio::time::advance(Duration::from_secs(20)).await;
  tokio::task::yield_now().await;
  assert!(!server.is_finished());
  ctl_ipc::write_frame(&mut channel, &Request::List)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, Response>(&mut channel)
      .await
      .unwrap(),
    Some(Response::Snapshot { snapshot }) if snapshot.connections.is_empty()
  ));
  server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn a_peer_that_never_selects_a_contract_expires_without_identity() {
  let (mut channel, server) = negotiation_channel();
  read_offer(&mut channel).await;
  tokio::time::advance(STARTUP_TIMEOUT.checked_sub(Duration::from_secs(1)).unwrap()).await;
  tokio::task::yield_now().await;
  assert!(!server.is_finished());
  tokio::time::advance(Duration::from_secs(1)).await;
  let error = server.await.unwrap().unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::TimedOut);
  assert_eq!(error.to_string(), "Remote VPN negotiation timed out");
  let mut remaining = Vec::new();
  channel.read_to_end(&mut remaining).await.unwrap();
  assert!(
    remaining.is_empty(),
    "identity requires a selected contract"
  );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn the_first_request_has_only_the_remaining_startup_budget() {
  let (mut channel, server) = negotiation_channel();
  read_offer(&mut channel).await;
  tokio::time::advance(STARTUP_TIMEOUT.checked_sub(Duration::from_secs(5)).unwrap()).await;
  select_contract(&mut channel).await;
  ctl_proto::read_identity(&mut channel).await.unwrap();
  tokio::time::advance(Duration::from_secs(4)).await;
  tokio::task::yield_now().await;
  assert!(!server.is_finished());
  tokio::time::advance(Duration::from_secs(1)).await;
  assert!(matches!(
    ctl_ipc::read_frame::<_, Response>(&mut channel)
      .await
      .unwrap(),
    Some(Response::Error { code, message })
      if code == "request_timeout" && message == "Remote VPN request timed out"
  ));
  server.await.unwrap().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn remote_connect_resolves_remote_status_preserves_hostname_and_relays_bytes() {
  let socket = std::env::temp_dir().join(format!(
    "ctl-agent-vpn-{}.sock",
    uuid::Uuid::new_v4().simple()
  ));
  let daemon = tokio::net::UnixListener::bind(&socket).unwrap();
  let socks = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let snapshot = VpnSnapshot {
    connections: vec![connected(&format!(
      "socks5h://{}",
      socks.local_addr().unwrap()
    ))],
    ..VpnSnapshot::default()
  };
  let owner = tokio::spawn(answer_status(daemon, snapshot));
  let proxy = tokio::spawn(async move {
    let mut stream = socks_destination(socks, b"unresolvable.internal", 2222).await;
    let mut payload = [0; 4];
    stream.read_exact(&mut payload).await.unwrap();
    assert_eq!(&payload, b"\0\xffab");
    stream.write_all(b"reply").await.unwrap();
  });
  let (mut channel, gateway) = tokio::io::duplex(1024);
  let client = ctl_ipc::vpn::Client::new(socket.clone());
  let identity = identity();
  let expected = identity.clone();
  let relay = tokio::spawn(async move {
    let (mut reader, mut writer) = tokio::io::split(gateway);
    serve(&mut reader, &mut writer, &client, &identity)
      .await
      .unwrap();
  });
  let mut preface = vec![0; PREFACE.len()];
  channel.read_exact(&mut preface).await.unwrap();
  assert_eq!(preface, PREFACE);
  let (mut reader, mut writer) = tokio::io::split(&mut channel);
  ctl_ipc::remote_vpn::negotiate_contract(&mut reader, &mut writer)
    .await
    .unwrap();
  assert_eq!(
    ctl_proto::read_identity(&mut reader).await.unwrap(),
    expected
  );
  drop((reader, writer));
  ctl_ipc::write_frame(
    &mut channel,
    &Request::Connect {
      connection_id: "work".into(),
      host: "unresolvable.internal".into(),
      port: 2222,
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, Response>(&mut channel)
      .await
      .unwrap(),
    Some(Response::Connected)
  ));
  channel.write_all(b"\0\xffab").await.unwrap();
  channel.shutdown().await.unwrap();
  let mut result = Vec::new();
  channel.read_to_end(&mut result).await.unwrap();
  assert_eq!(result, b"reply");
  tokio::time::timeout(Duration::from_secs(3), relay)
    .await
    .unwrap()
    .unwrap();
  owner.await.unwrap();
  proxy.await.unwrap();
  std::fs::remove_file(socket).unwrap();
}

#[cfg(unix)]
async fn answer_status(daemon: tokio::net::UnixListener, snapshot: VpnSnapshot) {
  let (mut stream, _) = daemon.accept().await.unwrap();
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
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut stream)
      .await
      .unwrap(),
    Some(ClientMessage::VpnStatus)
  ));
  ctl_ipc::write_frame(
    &mut stream,
    &ServerMessage::VpnStatus {
      status: snapshot.connections[0].clone().into(),
      snapshot: Some(snapshot),
    },
  )
  .await
  .unwrap();
}

#[cfg(unix)]
async fn socks_destination(
  socks: tokio::net::TcpListener,
  expected_host: &[u8],
  expected_port: u16,
) -> tokio::net::TcpStream {
  let (mut stream, _) = socks.accept().await.unwrap();
  let mut greeting = [0; 3];
  stream.read_exact(&mut greeting).await.unwrap();
  assert_eq!(greeting, [5, 1, 0]);
  stream.write_all(&[5, 0]).await.unwrap();
  let mut request = [0; 5];
  stream.read_exact(&mut request).await.unwrap();
  assert_eq!(request[..4], [5, 1, 0, 3]);
  let mut host = vec![0; usize::from(request[4])];
  stream.read_exact(&mut host).await.unwrap();
  assert_eq!(host, expected_host);
  assert_eq!(stream.read_u16().await.unwrap(), expected_port);
  stream
    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
    .await
    .unwrap();
  stream
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_vpn_child() {
  let Some(socket) = std::env::var_os("CTL_AGENT_VPN_TEST_SOCKET") else {
    return;
  };
  serve_stdio(&ctl_ipc::vpn::Client::new(socket.into()), &identity())
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn socketpair_stdio_negotiates_a_fragmented_selection_before_the_request() {
  use std::os::fd::OwnedFd;
  use std::process::Stdio;

  let socket = std::env::temp_dir().join(format!("cvs-{}.s", uuid::Uuid::new_v4().simple()));
  let (local, peer) = std::os::unix::net::UnixStream::pair().unwrap();
  let input: OwnedFd = local.try_clone().unwrap().into();
  let output: OwnedFd = local.into();
  let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "vpn::tests::stdio_vpn_child", "--nocapture"])
    .env("CTL_AGENT_VPN_TEST_SOCKET", &socket)
    .stdin(Stdio::from(input))
    .stdout(Stdio::from(output))
    .stderr(Stdio::inherit())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  peer.set_nonblocking(true).unwrap();
  let mut channel = tokio::net::UnixStream::from_std(peer).unwrap();
  tokio::time::timeout(Duration::from_secs(3), async {
    // The test runner banner precedes the protocol's owned standard output.
    let mut banner = Vec::new();
    while !banner.ends_with(PREFACE) {
      banner.push(channel.read_u8().await.unwrap());
      assert!(banner.len() < 1024);
    }
    let offer: ctl_core::protocol::ProtocolOffer =
      ctl_ipc::read_frame(&mut channel).await.unwrap().unwrap();
    assert_eq!(offer, ctl_ipc::remote_vpn::protocol_offer());
    assert!(child.try_wait().unwrap().is_none());
    let mut selection = Vec::new();
    select_contract(&mut selection).await;
    for byte in selection {
      channel.write_all(&[byte]).await.unwrap();
      tokio::task::yield_now().await;
    }
    assert!(
      ctl_proto::read_identity(&mut channel)
        .await
        .unwrap()
        .is_valid()
    );
    ctl_ipc::write_frame(&mut channel, &Request::List)
      .await
      .unwrap();
    assert!(matches!(
      ctl_ipc::read_frame::<_, Response>(&mut channel)
        .await
        .unwrap(),
      Some(Response::Snapshot { snapshot }) if snapshot.connections.is_empty()
    ));
    let mut remaining = Vec::new();
    channel.read_to_end(&mut remaining).await.unwrap();
    assert_eq!(remaining, [] as [u8; 0]);
    assert!(child.wait().await.unwrap().success());
  })
  .await
  .expect("shared SSH descriptors must complete negotiation and the request");
  assert!(!socket.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_vpn_delivers_target_eof_before_client_finishes_uploading() {
  let socket = std::env::temp_dir().join(format!("cvs-{}.s", uuid::Uuid::new_v4().simple()));
  let daemon = tokio::net::UnixListener::bind(&socket).unwrap();
  let socks = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let snapshot = VpnSnapshot {
    connections: vec![connected(&format!(
      "socks5h://{}",
      socks.local_addr().unwrap()
    ))],
    ..VpnSnapshot::default()
  };
  let owner = tokio::spawn(answer_status(daemon, snapshot));
  let proxy = tokio::spawn(async move {
    let mut stream = socks_destination(socks, b"internal.test", 443).await;
    stream.write_all(b"reply-before-upload").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut upload = Vec::new();
    stream.read_to_end(&mut upload).await.unwrap();
    assert_eq!(upload, b"upload-after-output-eof");
  });
  let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
    .args(["--exact", "vpn::tests::stdio_vpn_child", "--nocapture"])
    .env("CTL_AGENT_VPN_TEST_SOCKET", &socket)
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::inherit())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let mut input = child.stdin.take().unwrap();
  let mut output = child.stdout.take().unwrap();
  // The child test runner prints a banner before the process takes stdio.
  let mut banner = Vec::new();
  while !banner.ends_with(PREFACE) {
    banner.push(output.read_u8().await.unwrap());
    assert!(banner.len() < 1024);
  }
  ctl_ipc::remote_vpn::negotiate_contract(&mut output, &mut input)
    .await
    .unwrap();
  ctl_proto::read_identity(&mut output).await.unwrap();
  ctl_ipc::write_frame(
    &mut input,
    &Request::Connect {
      connection_id: "work".into(),
      host: "internal.test".into(),
      port: 443,
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, Response>(&mut output)
      .await
      .unwrap(),
    Some(Response::Connected)
  ));
  let mut reply = Vec::new();
  tokio::time::timeout(Duration::from_secs(3), output.read_to_end(&mut reply))
    .await
    .expect("target write EOF must reach the client with client stdin still open")
    .unwrap();
  assert_eq!(reply, b"reply-before-upload");
  assert!(child.try_wait().unwrap().is_none());
  input.write_all(b"upload-after-output-eof").await.unwrap();
  drop(input);
  assert!(
    tokio::time::timeout(Duration::from_secs(3), child.wait())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
  owner.await.unwrap();
  proxy.await.unwrap();
  std::fs::remove_file(socket).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn list_on_absent_owner_never_starts_the_companion_and_full_broker_requests_are_rejected() {
  let socket = std::env::temp_dir().join(format!(
    "ctl-agent-absent-{}.sock",
    uuid::Uuid::new_v4().simple()
  ));
  for request in [
    serde_json::json!({"type":"list"}),
    serde_json::json!({"type":"delete_credentials","target":{}}),
  ] {
    let client = ctl_ipc::vpn::Client::new(socket.clone())
      .with_daemon_executable("/does/not/exist/ctld".into());
    let mut input = Vec::new();
    ctl_ipc::write_frame(
      &mut input,
      &ctl_ipc::remote_vpn::ProtocolSelection {
        protocol_version: ctl_ipc::remote_vpn::CONTRACT_V1_0_1,
      },
    )
    .await
    .unwrap();
    ctl_ipc::write_frame(&mut input, &request).await.unwrap();
    let mut output = Vec::new();
    serve(&mut input.as_slice(), &mut output, &client, &identity())
      .await
      .unwrap();
    let mut output = &output[PREFACE.len()..];
    ctl_ipc::remote_vpn::negotiate_contract(&mut output, &mut input)
      .await
      .unwrap();
    ctl_proto::read_identity(&mut output).await.unwrap();
    match ctl_ipc::read_frame::<_, Response>(&mut output)
      .await
      .unwrap()
      .unwrap()
    {
      Response::Snapshot { snapshot } => assert_eq!(snapshot.connections, []),
      Response::Error { code, .. } => assert_eq!(code, "invalid_request"),
      _ => panic!("unexpected response"),
    }
    assert!(!socket.exists());
  }
}
