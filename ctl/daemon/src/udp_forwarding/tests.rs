use super::*;
#[cfg(unix)]
use ctl_ipc::PortForwardProtocol;
use std::time::Duration;
use tokio::time::timeout;

fn target() -> SshTarget {
  serde_json::from_value(serde_json::json!({"destination": "fixture.invalid"})).unwrap()
}

#[cfg(unix)]
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

#[tokio::test]
async fn blocked_rules_share_upload_bytes_until_writes_finish_or_are_cancelled() {
  const PACKET_BYTES: usize = 1024;
  let mut registry = Registry::default();
  assert_eq!(registry.upload_budget.available_permits(), MAX_UPLOAD_BYTES);
  registry.upload_budget = Arc::new(Semaphore::new(4 * PACKET_BYTES));
  let budget = Arc::clone(&registry.upload_budget);
  let (first_outgoing, first_uploads) = mpsc::channel(4);
  let (second_outgoing, second_uploads) = mpsc::channel(4);
  let payload = vec![42; PACKET_BYTES];
  for (outgoing, peer_ids) in [(&first_outgoing, [1, 2]), (&second_outgoing, [3, 4])] {
    for peer_id in peer_ids {
      assert!(try_queue_datagram(outgoing, &budget, peer_id, &payload));
    }
  }
  assert_eq!(budget.available_permits(), 0);

  // Each writer can send only the frame kind before blocking. Taking a frame
  // out of its queue must keep the payload charged while that write is pending.
  let (mut first_writer, mut first_remote) = tokio::io::duplex(1);
  let (mut second_writer, mut second_remote) = tokio::io::duplex(1);
  let first = tokio::spawn(async move { write_uploads(&mut first_writer, first_uploads).await });
  let second = tokio::spawn(async move { write_uploads(&mut second_writer, second_uploads).await });
  assert_eq!(
    timeout(Duration::from_secs(3), first_remote.read_u8())
      .await
      .unwrap()
      .unwrap(),
    1
  );
  let kind = timeout(Duration::from_secs(3), second_remote.read_u8())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(kind, 1);
  assert_eq!(budget.available_permits(), 0);
  assert!(!try_queue_datagram(&second_outgoing, &budget, 99, &payload));
  // Peer retirement still has a queue slot when all shared payload bytes are
  // in use. It requires no byte permit and must follow the preceding datagrams.
  second_outgoing.try_send(Upload::close(4)).unwrap();
  assert_eq!(budget.available_permits(), 0);

  first.abort();
  assert!(first.await.unwrap_err().is_cancelled());
  assert_eq!(budget.available_permits(), 2 * PACKET_BYTES);
  assert!(try_queue_datagram(&second_outgoing, &budget, 5, &payload));
  assert_eq!(budget.available_permits(), PACKET_BYTES);
  drop(second_outgoing);

  let mut second_remote = std::io::Cursor::new([kind]).chain(second_remote);
  for peer_id in [3, 4] {
    assert_eq!(
      next_packet(&mut second_remote).await,
      forwarding::Frame::Datagram {
        peer_id,
        payload: payload.clone()
      }
    );
  }
  assert_eq!(
    next_packet(&mut second_remote).await,
    forwarding::Frame::Close { peer_id: 4 }
  );
  assert_eq!(
    next_packet(&mut second_remote).await,
    forwarding::Frame::Datagram {
      peer_id: 5,
      payload
    }
  );
  timeout(Duration::from_secs(3), second)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  assert_eq!(budget.available_permits(), 4 * PACKET_BYTES);
  assert!(
    forwarding::read_packet(&mut second_remote)
      .await
      .unwrap()
      .is_none()
  );
}

#[test]
fn a_full_rule_queue_does_not_reserve_upload_bytes_from_siblings() {
  let budget = Arc::new(Semaphore::new(8));
  let (first, first_uploads) = mpsc::channel(1);
  let (second, second_uploads) = mpsc::channel(1);
  assert!(try_queue_datagram(&first, &budget, 1, b"data"));
  assert_eq!(budget.available_permits(), 4);
  assert!(!try_queue_datagram(&first, &budget, 1, b"drop"));
  assert_eq!(budget.available_permits(), 4);
  assert!(try_queue_datagram(&second, &budget, 1, b"data"));
  assert_eq!(budget.available_permits(), 0);
  drop(first_uploads);
  drop(second_uploads);
  assert_eq!(budget.available_permits(), 8);
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
    stdin: Box::new(process.stdin().unwrap()),
    stdout: Box::new(process.stdout().unwrap()),
    process: Some(process),
  })
}

#[cfg(unix)]
#[tokio::test]
async fn local_clients_keep_datagram_boundaries_and_cancellation_releases_the_socket() {
  let mut registry = Registry::default();
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let forward = forward(address.port());
  let listener = registry.listener(socket, echo_channel().unwrap());
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
  assert_socket_released(address).await;
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
      &super::super::agent_transport::Registry::default(),
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
      process: Some(process),
      stdin: Box::new(stdin),
      stdout: Box::new(stdout),
    },
    pid,
  )
}

async fn assert_socket_released(address: SocketAddr) {
  timeout(Duration::from_secs(3), async {
    loop {
      match UdpSocket::bind(address).await {
        Ok(_) => break,
        // Another parallel test can claim a released ephemeral port before
        // this probe. Keep the release check bounded, as with process exit.
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
          tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err(error) => panic!("could not probe the released UDP socket: {error}"),
      }
    }
  })
  .await
  .expect("cancelling UDP ownership must release its socket");
}

#[cfg(unix)]
async fn assert_released(address: SocketAddr, pids: &[u32]) {
  assert_socket_released(address).await;
  timeout(Duration::from_secs(3), async {
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
  .expect("cancelling UDP ownership must release its SSH processes");
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_uncommitted_udp_startup_releases_idle_and_active_channels() {
  let registry = Registry::default();
  for active_peers in [false, true] {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let (initial, pid) = echo_channel_with_pid().await;
    let listener = registry.listener(socket, initial);
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
  let listener = state.udp_forwards.lock().await.listener(socket, initial);
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
  let registry = Registry::default();
  let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
  let address = socket.local_addr().unwrap();
  let (channel, pid) = echo_channel_with_pid().await;
  let listener = registry.listener(socket, channel);
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
    relay_socket(
      &socket,
      &mut writer,
      &mut reader,
      receiver,
      Registry::default().upload_budget,
    )
    .await
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
  assert_socket_released(address).await;
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
    relay_socket(
      &socket,
      &mut writer,
      &mut reader,
      receiver,
      Registry::default().upload_budget,
    )
    .await
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
  assert_socket_released(address).await;
}
