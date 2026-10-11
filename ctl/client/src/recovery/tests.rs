use super::*;

fn target() -> SshTarget {
  SshTarget {
    destination: "work".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

async fn accept(
  stream: &mut tokio::io::DuplexStream,
  protocol: ctl_core::protocol::ProtocolVersion,
) {
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(stream)
      .await
      .unwrap(),
    Some(ClientMessage::Handshake { .. })
  ));
  ctl_ipc::write_frame(
    stream,
    &ServerMessage::HandshakeAccepted {
      protocol_version: protocol,
    },
  )
  .await
  .unwrap();
}

#[tokio::test]
async fn old_contract_never_receives_a_registration_request() {
  let (mut client, mut server) = tokio::io::duplex(4096);
  let server = tokio::spawn(async move {
    accept(&mut server, ctl_ipc::CONTRACT_V1_1_15).await;
    assert!(
      ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
        .await
        .unwrap()
        .is_none()
    );
  });
  assert!(!register(&mut client, target()).await.unwrap());
  drop(client);
  server.await.unwrap();
}

#[tokio::test]
async fn registration_retains_the_exact_route_and_counts_authorization_changes() {
  let (mut client, mut server) = tokio::io::duplex(4096);
  let server = tokio::spawn(async move {
    accept(&mut server, ctl_ipc::PROTOCOL_VERSION).await;
    let Some(ClientMessage::WatchRecovery { target: registered }) =
      ctl_ipc::read_frame(&mut server).await.unwrap()
    else {
      panic!("expected recovery registration");
    };
    assert_eq!(registered, target());
    ctl_ipc::write_frame(&mut server, &ServerMessage::RecoveryRegistered)
      .await
      .unwrap();
    for generation in [2, 2, 3] {
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::RecoveryAuthorized { generation },
      )
      .await
      .unwrap();
    }
  });
  let generation = AtomicU64::new(10);
  assert!(register(&mut client, target()).await.unwrap());
  read_authorizations(&mut client, &generation).await.unwrap();
  assert_eq!(generation.load(Ordering::Acquire), 12);
  server.await.unwrap();
}

#[tokio::test]
async fn dropping_a_registration_closes_its_socket_and_stops_notifications() {
  let (mut client, mut server) = tokio::io::duplex(4096);
  let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
  let generation = Arc::new(AtomicU64::new(0));
  let worker_generation = Arc::clone(&generation);
  let watch = RecoveryWatch {
    target: target(),
    worker: tokio::spawn(async move {
      assert!(register(&mut client, target()).await.unwrap());
      registered_tx.send(()).unwrap();
      read_authorizations(&mut client, &worker_generation)
        .await
        .unwrap();
    }),
  };
  accept(&mut server, ctl_ipc::PROTOCOL_VERSION).await;
  assert!(matches!(
    ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
      .await
      .unwrap(),
    Some(ClientMessage::WatchRecovery { .. })
  ));
  ctl_ipc::write_frame(&mut server, &ServerMessage::RecoveryRegistered)
    .await
    .unwrap();
  registered_rx.await.unwrap();
  drop(watch);
  assert!(
    tokio::time::timeout(
      Duration::from_secs(1),
      ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
    )
    .await
    .unwrap()
    .unwrap()
    .is_none()
  );
  assert_eq!(generation.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn events_before_registration_do_not_authorize_the_frontend() {
  let (mut client, mut server) = tokio::io::duplex(4096);
  let server = tokio::spawn(async move {
    accept(&mut server, ctl_ipc::PROTOCOL_VERSION).await;
    let _ = ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
      .await
      .unwrap();
    ctl_ipc::write_frame(
      &mut server,
      &ServerMessage::RecoveryAuthorized { generation: 1 },
    )
    .await
    .unwrap();
  });
  assert!(matches!(
    register(&mut client, target()).await,
    Err(Error::UnexpectedResponse)
  ));
  server.await.unwrap();
}
