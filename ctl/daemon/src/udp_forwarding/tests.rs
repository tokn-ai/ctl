use super::*;
use ctl_ipc::PortForwardProtocol;
use std::time::Duration;
use tokio::time::timeout;

fn target() -> SshTarget {
  serde_json::from_value(serde_json::json!({"destination": "fixture.invalid"})).unwrap()
}

fn forward(port: u16) -> LocalPortForward {
  LocalPortForward {
    protocol: PortForwardProtocol::Udp,
    forward_id: "udp-fixture".into(),
    bind_address: "127.0.0.1".into(),
    local_port: port,
    remote_host: "127.0.0.1".into(),
    remote_port: 53,
  }
}

async fn next_packet(reader: &mut (impl AsyncRead + Unpin)) -> forwarding::Frame {
  timeout(Duration::from_secs(3), forwarding::read_packet(reader))
    .await
    .unwrap()
    .unwrap()
    .unwrap()
}

async fn next_local_datagram(socket: &UdpSocket) -> Vec<u8> {
  let mut payload = vec![0; forwarding::MAX_DATAGRAM_BYTES];
  let (size, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut payload))
    .await
    .unwrap()
    .unwrap();
  payload.truncate(size);
  payload
}

#[cfg(unix)]
fn echo_channel() -> io::Result<Channel> {
  let mut command = Command::new("cat");
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut process = Process::spawn(command)?;
  Ok(Channel {
    stdin: process.stdin().unwrap(),
    stdout: process.stdout().unwrap(),
    process,
  })
}

#[cfg(unix)]
#[tokio::test]
async fn local_clients_keep_datagram_boundaries_and_cancellation_releases_the_socket() {
  let mut registry = Registry::default();
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let forward = forward(address.port());
  let listener = Registry::listener(socket, echo_channel().unwrap());
  registry
    .listeners
    .insert((target(), forward.clone()), listener);
  registry.check(&target(), &forward).unwrap();
  let first = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let second = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  first.connect(address).await.unwrap();
  second.connect(address).await.unwrap();
  // macOS limits real UDP writes below the protocol's maximum framing size.
  for bytes in [vec![], vec![0, 255, 10], vec![42; 8000]] {
    first.send(&bytes).await.unwrap();
    second.send(b"other peer").await.unwrap();
    let mut buffer = vec![0; forwarding::MAX_DATAGRAM_BYTES];
    let count = timeout(Duration::from_secs(3), second.recv(&mut buffer))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&buffer[..count], b"other peer");
    let count = timeout(Duration::from_secs(3), first.recv(&mut buffer))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&buffer[..count], bytes);
  }
  let other = LocalPortForward {
    remote_port: 54,
    ..forward.clone()
  };
  registry.cancel(&target(), &other).await;
  registry.check(&target(), &forward).unwrap();
  registry.cancel(&target(), &forward).await;
  assert!(registry.check(&target(), &forward).is_err());
  assert!(UdpSocket::bind(address).await.is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn udp_collision_does_not_open_a_channel_or_disturb_tcp() {
  let external = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let forward = forward(external.local_addr().unwrap().port());
  let tcp = tokio::net::TcpListener::bind(("127.0.0.1", forward.local_port))
    .await
    .unwrap();
  let mut registry = Registry::default();
  let error = registry
    .prepare(
      &target(),
      Some(Path::new("/fixture/missing-control")),
      &forward,
    )
    .await
    .err()
    .unwrap();
  assert!(error.to_string().contains("already in use"));
  assert!(registry.listeners.is_empty());
  assert_eq!(tcp.local_addr().unwrap().port(), forward.local_port);
}

#[test]
fn udp_ssh_command_reuses_only_the_selected_master_and_never_shell_interpolates_a_destination() {
  let command = command(&target(), Some(Path::new("/fixture/control")));
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|arg| arg.to_string_lossy())
    .collect();
  assert!(args.iter().any(|arg| arg == "ProxyCommand=false"));
  assert!(args.iter().any(|arg| arg == "ClearAllForwardings=yes"));
  assert!(args.iter().any(|arg| arg == "PermitLocalCommand=no"));
  assert!(args.iter().all(|arg| !arg.contains("untrusted")));
  assert!(args.last().unwrap().ends_with("exec ctl-agent forward-udp"));
}

#[cfg(unix)]
async fn echo_channel_with_pid() -> (Channel, u32) {
  let mut command = Command::new("/bin/sh");
  command
    .args(["-c", "printf '%s\\n' \"$$\"; exec cat"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut process = Process::spawn(command).unwrap();
  let stdin = process.stdin().unwrap();
  let mut stdout = process.stdout().unwrap();
  let pid = timeout(Duration::from_secs(3), async {
    let mut bytes = Vec::new();
    loop {
      let byte = stdout.read_u8().await.unwrap();
      if byte == b'\n' {
        break String::from_utf8(bytes).unwrap().parse::<u32>().unwrap();
      }
      assert!(bytes.len() < 16);
      bytes.push(byte);
    }
  })
  .await
  .unwrap();
  (
    Channel {
      process,
      stdin,
      stdout,
    },
    pid,
  )
}

#[cfg(unix)]
async fn assert_released(address: SocketAddr, pids: &[u32]) {
  timeout(Duration::from_secs(3), async {
    loop {
      if let Ok(socket) = UdpSocket::bind(address).await {
        drop(socket);
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for pid in pids {
      while Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap()
        .success()
      {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    }
  })
  .await
  .expect("cancelling UDP ownership must release its socket and SSH processes");
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_uncommitted_udp_startup_releases_idle_and_active_channels() {
  for active_peers in [false, true] {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let (initial, pid) = echo_channel_with_pid().await;
    let listener = Registry::listener(socket, initial);
    let prepared = Prepared {
      key: (target(), forward(address.port())),
      listener,
    };
    if active_peers {
      let mut clients = Vec::new();
      for payload in [b"first".as_slice(), b"second".as_slice()] {
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(payload, address).await.unwrap();
        let mut bytes = [0; 32];
        let (count, _) = timeout(Duration::from_secs(3), client.recv_from(&mut bytes))
          .await
          .unwrap()
          .unwrap();
        assert_eq!(&bytes[..count], payload);
        clients.push(client);
      }
    }
    drop(prepared);
    assert_released(address, &[pid]).await;
  }
}

#[cfg(unix)]
#[tokio::test]
async fn disconnecting_a_missing_private_master_releases_owned_udp_listeners() {
  let state = crate::State::default();
  let target = SshTarget {
    destination: uuid::Uuid::new_v4().to_string(),
    ..target()
  };
  assert!(!crate::control_path(&target).exists());
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let (initial, pid) = echo_channel_with_pid().await;
  let listener = Registry::listener(socket, initial);
  state
    .udp_forwards
    .lock()
    .await
    .listeners
    .insert((target.clone(), forward(address.port())), listener);

  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  crate::disconnect_master(&mut server, &state, &target)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, crate::ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(crate::ServerMessage::MasterDisconnected),
  ));
  assert!(state.udp_forwards.lock().await.listeners.is_empty());
  assert_released(address, &[pid]).await;
}

#[cfg(unix)]
#[tokio::test]
async fn more_than_ten_local_clients_share_one_channel_and_remain_isolated() {
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let (channel, pid) = echo_channel_with_pid().await;
  let listener = Registry::listener(socket, channel);
  let mut clients = Vec::new();
  for index in 0_u8..16 {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect(address).await.unwrap();
    client.send(&[index]).await.unwrap();
    clients.push(client);
  }
  for (index, client) in clients.iter().enumerate() {
    let mut packet = [0; 16];
    let size = timeout(Duration::from_secs(3), client.recv(&mut packet))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(&packet[..size], &[u8::try_from(index).unwrap()]);
  }
  // No callback or process factory exists on the listener: all 16 peer IDs
  // travel through the one owned process created before the first packet.
  listener.stop().await;
  assert_released(address, &[pid]).await;
}

#[tokio::test]
async fn remote_close_retires_a_peer_and_stale_replies_cannot_reach_its_replacement() {
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let (daemon, remote) = tokio::io::duplex(4096);
  let (mut remote_reader, mut remote_writer) = tokio::io::split(remote);
  let (stop, receiver) = oneshot::channel();
  let relay = tokio::spawn(async move {
    let (mut reader, mut writer) = tokio::io::split(daemon);
    relay_socket(&socket, &mut writer, &mut reader, receiver).await
  });
  let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  client.send_to(b"first", address).await.unwrap();
  let first = next_packet(&mut remote_reader).await;
  let forwarding::Frame::Datagram {
    peer_id: first_id,
    payload,
  } = first
  else {
    panic!("expected data");
  };
  assert_eq!(payload, b"first");
  forwarding::write_packet(
    &mut remote_writer,
    &forwarding::Frame::Close { peer_id: first_id },
  )
  .await
  .unwrap();
  // A response for an independent peer is a barrier proving the close frame
  // was consumed before the first source port sends again.
  let barrier = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  barrier.send_to(b"barrier", address).await.unwrap();
  let packet = next_packet(&mut remote_reader).await;
  forwarding::write_packet(&mut remote_writer, &packet)
    .await
    .unwrap();
  let mut bytes = [0; 32];
  timeout(Duration::from_secs(3), barrier.recv_from(&mut bytes))
    .await
    .unwrap()
    .unwrap();
  client.send_to(b"second", address).await.unwrap();
  let replacement = next_packet(&mut remote_reader).await;
  let forwarding::Frame::Datagram {
    peer_id: replacement_id,
    payload,
  } = &replacement
  else {
    panic!("expected data");
  };
  assert!(*replacement_id > first_id);
  assert_eq!(payload, b"second");
  forwarding::write_packet(
    &mut remote_writer,
    &forwarding::Frame::Datagram {
      peer_id: first_id,
      payload: b"stale".to_vec(),
    },
  )
  .await
  .unwrap();
  forwarding::write_packet(&mut remote_writer, &replacement)
    .await
    .unwrap();
  let (size, _) = timeout(Duration::from_secs(3), client.recv_from(&mut bytes))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&bytes[..size], b"second");
  stop.send(()).unwrap();
  relay.await.unwrap().unwrap();
  assert!(UdpSocket::bind(address).await.is_ok());
}

#[tokio::test]
async fn closing_a_peer_releases_full_local_capacity_without_reusing_its_id() {
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let (daemon, remote) = tokio::io::duplex(4096);
  let (mut remote_reader, mut remote_writer) = tokio::io::split(remote);
  let (stop, receiver) = oneshot::channel();
  let relay = tokio::spawn(async move {
    let (mut reader, mut writer) = tokio::io::split(daemon);
    relay_socket(&socket, &mut writer, &mut reader, receiver).await
  });
  let mut clients = Vec::new();
  let mut peer_ids = Vec::new();
  for index in 0..forwarding::MAX_PEERS {
    let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let payload = [u8::try_from(index).unwrap()];
    client.send_to(&payload, address).await.unwrap();
    let packet = next_packet(&mut remote_reader).await;
    let forwarding::Frame::Datagram {
      peer_id,
      payload: actual,
    } = packet
    else {
      panic!("expected a newly admitted peer");
    };
    assert_eq!(actual, payload);
    assert!(peer_ids.last().is_none_or(|previous| peer_id > *previous));
    clients.push(client);
    peer_ids.push(peer_id);
  }

  let replacement = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  replacement
    .send_to(b"over capacity", address)
    .await
    .unwrap();
  // The next admitted packet is from an existing peer: the extra source sent
  // first but must produce no frame while all local associations are occupied.
  clients[1]
    .send_to(b"capacity barrier", address)
    .await
    .unwrap();
  assert_eq!(
    next_packet(&mut remote_reader).await,
    forwarding::Frame::Datagram {
      peer_id: peer_ids[1],
      payload: b"capacity barrier".to_vec(),
    }
  );

  forwarding::write_packet(
    &mut remote_writer,
    &forwarding::Frame::Close {
      peer_id: peer_ids[0],
    },
  )
  .await
  .unwrap();
  clients[1].send_to(b"barrier", address).await.unwrap();
  let barrier = next_packet(&mut remote_reader).await;
  assert_eq!(
    barrier,
    forwarding::Frame::Datagram {
      peer_id: peer_ids[1],
      payload: b"barrier".to_vec(),
    }
  );
  forwarding::write_packet(&mut remote_writer, &barrier)
    .await
    .unwrap();
  assert_eq!(next_local_datagram(&clients[1]).await, b"barrier");

  replacement.send_to(b"replacement", address).await.unwrap();
  let packet = next_packet(&mut remote_reader).await;
  let forwarding::Frame::Datagram { peer_id, payload } = &packet else {
    panic!("expected the freed capacity to admit a replacement");
  };
  assert!(*peer_id > *peer_ids.last().unwrap());
  assert_eq!(payload, b"replacement");
  forwarding::write_packet(
    &mut remote_writer,
    &forwarding::Frame::Datagram {
      peer_id: peer_ids[0],
      payload: b"stale".to_vec(),
    },
  )
  .await
  .unwrap();
  forwarding::write_packet(&mut remote_writer, &packet)
    .await
    .unwrap();
  assert_eq!(next_local_datagram(&replacement).await, b"replacement");
  assert!(
    timeout(Duration::from_millis(100), next_local_datagram(&clients[0]))
      .await
      .is_err()
  );

  stop.send(()).unwrap();
  relay.await.unwrap().unwrap();
  assert!(UdpSocket::bind(address).await.is_ok());
}
