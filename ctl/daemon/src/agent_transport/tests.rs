use super::*;
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::AsyncWriteExt as _;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::timeout;

struct Directory(PathBuf);

impl Drop for Directory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn directory() -> Directory {
  let directory =
    Directory(PathBuf::from("/tmp").join(format!("ctl-pool-{}", uuid::Uuid::new_v4().simple())));
  std::fs::DirBuilder::new()
    .mode(0o700)
    .create(&directory.0)
    .unwrap();
  directory
}

fn target(destination: &str) -> SshTarget {
  serde_json::from_value(serde_json::json!({"destination": destination})).unwrap()
}

async fn echo_gateway(starts: Arc<AtomicUsize>, stopped: mpsc::Sender<()>) -> io::Result<Client> {
  starts.fetch_add(1, Ordering::Relaxed);
  let (client, server) = tokio::io::duplex(64 * 1024);
  tokio::spawn(async move {
    let _ = transport::serve(server, |_, stream| async move {
      let (mut reader, mut writer) = tokio::io::split(stream);
      tokio::io::copy(&mut reader, &mut writer).await?;
      writer.shutdown().await
    })
    .await;
    let _ = stopped.send(()).await;
  });
  transport::connect(client).await
}

#[tokio::test]
async fn views_and_rules_share_initialization_and_master_replacement_closes_old_streams() {
  let directory = directory();
  let path = directory.0.join("master");
  let old_socket = UnixListener::bind(&path).unwrap();
  let target: SshTarget =
    serde_json::from_value(serde_json::json!({"destination":"fixture.invalid"})).unwrap();
  let registry = Arc::new(Registry::default());
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(2);
  let mut opens = JoinSet::new();
  for index in 0_u8..32 {
    let registry = Arc::clone(&registry);
    let mut target = target.clone();
    target.destination = format!("fixture-{index}.invalid");
    let path = path.clone();
    let starts = Arc::clone(&starts);
    let stopped = stopped.clone();
    opens.spawn(async move {
      let service = if index % 2 == 0 {
        Service::Ctmux
      } else {
        Service::Udp
      };
      let mut stream = registry
        .open_with(&target, &path, service, || echo_gateway(starts, stopped))
        .await
        .unwrap();
      stream.write_all(&[index]).await.unwrap();
      stream.flush().await.unwrap();
      assert_eq!(stream.read_u8().await.unwrap(), index);
      stream
    });
  }
  let mut streams = Vec::new();
  while let Some(stream) = opens.join_next().await {
    streams.push(stream.unwrap());
  }
  assert_eq!(starts.load(Ordering::Relaxed), 1);
  let mut first_alias = target.clone();
  first_alias.destination = "fixture-0.invalid".into();
  registry.cancel_target(&first_alias);
  // Disconnecting one alias must not close the shared physical gateway while
  // another alias still owns logical services on the same control socket.
  streams[1].write_all(b"sibling").await.unwrap();
  streams[1].flush().await.unwrap();
  let mut sibling = [0; 7];
  streams[1].read_exact(&mut sibling).await.unwrap();
  assert_eq!(&sibling, b"sibling");

  // Keep the original inode alive while replacing its pathname, as when a
  // configured master is replaced without waiting for the prior SSH to exit.
  std::fs::remove_file(&path).unwrap();
  let replacement_socket = UnixListener::bind(&path).unwrap();
  let mut replacement = registry
    .open_with(&target, &path, Service::Task, || {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 2);
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  for stream in &mut streams {
    assert!(
      timeout(Duration::from_secs(2), stream.read_u8())
        .await
        .unwrap()
        .is_err()
    );
  }
  replacement.write_all(b"new master").await.unwrap();
  replacement.flush().await.unwrap();
  let mut echoed = [0; 10];
  replacement.read_exact(&mut echoed).await.unwrap();
  assert_eq!(&echoed, b"new master");
  registry.cancel_target(&target);
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(
    timeout(Duration::from_secs(2), replacement.read_u8())
      .await
      .unwrap()
      .is_err()
  );
  drop((old_socket, replacement_socket));
}

#[tokio::test]
async fn disconnect_never_waits_for_an_unrelated_hosts_initializer() {
  let directory = directory();
  let ready_path = directory.0.join("ready");
  let pending_path = directory.0.join("pending");
  let _ready_socket = UnixListener::bind(&ready_path).unwrap();
  let _pending_socket = UnixListener::bind(&pending_path).unwrap();
  let ready_target = target("ready.invalid");
  let pending_target = target("pending.invalid");
  let registry = Arc::new(Registry::default());
  let (stopped, mut stops) = mpsc::channel(1);
  let mut ready = registry
    .open_with(&ready_target, &ready_path, Service::Ctmux, || {
      echo_gateway(Arc::default(), stopped)
    })
    .await
    .unwrap();
  let (started, initializing) = tokio::sync::oneshot::channel();
  let pending_registry = Arc::clone(&registry);
  let pending = tokio::spawn(async move {
    pending_registry
      .open_with(&pending_target, &pending_path, Service::Task, || async {
        let _ = started.send(());
        std::future::pending::<io::Result<Client>>().await
      })
      .await
  });
  initializing.await.unwrap();
  registry.cancel_target(&ready_target);
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(ready.read_u8().await.is_err());
  assert!(!pending.is_finished());
  registry.shutdown();
  let error = timeout(Duration::from_secs(1), pending)
    .await
    .unwrap()
    .unwrap()
    .err()
    .expect("shutdown cancels the blocked initializer");
  assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
}

#[tokio::test]
async fn cancelling_an_alias_initializer_releases_its_siblings_startup() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let first_target = target("first.invalid");
  let next_target = target("next.invalid");
  let registry = Arc::new(Registry::default());
  let (started, initializing) = tokio::sync::oneshot::channel();
  let first_registry = Arc::clone(&registry);
  let first_path = path.clone();
  let first_owner = first_target.clone();
  let first = tokio::spawn(async move {
    first_registry
      .open_with(&first_owner, &first_path, Service::Ctmux, || async {
        let _ = started.send(());
        std::future::pending::<io::Result<Client>>().await
      })
      .await
  });
  initializing.await.unwrap();
  let (stopped, mut stops) = mpsc::channel(1);
  let next_registry = Arc::clone(&registry);
  let next_path = path.clone();
  let next_owner = next_target.clone();
  let next = tokio::spawn(async move {
    next_registry
      .open_with(&next_owner, &next_path, Service::Task, || {
        echo_gateway(Arc::default(), stopped)
      })
      .await
  });
  timeout(Duration::from_secs(1), async {
    loop {
      let both_registered = registry
        .gateways
        .lock()
        .unwrap()
        .values()
        .any(|slot| slot.owned.lock().unwrap().targets.len() == 2);
      if both_registered {
        break;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
  registry.cancel_target(&first_target);
  assert_eq!(
    first
      .await
      .unwrap()
      .err()
      .expect("cancelled initializer")
      .kind(),
    io::ErrorKind::ConnectionAborted
  );
  let mut next = timeout(Duration::from_secs(1), next)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  next.write_all(b"healthy").await.unwrap();
  let mut bytes = [0; 7];
  next.read_exact(&mut bytes).await.unwrap();
  assert_eq!(&bytes, b"healthy");
  registry.cancel_target(&next_target);
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(registry.gateways.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_initializer_finishing_with_disconnect_cannot_publish_a_detached_gateway() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let target = target("fixture.invalid");
  let registry = Registry::default();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(2);
  let error = registry
    .open_with(&target, &path, Service::Ctmux, || async {
      let client = echo_gateway(Arc::clone(&starts), stopped.clone()).await?;
      registry.cancel_target(&target);
      Ok(client)
    })
    .await
    .err()
    .expect("initializer completed during disconnect");
  assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
  assert!(registry.gateways.lock().unwrap().is_empty());
  timeout(Duration::from_secs(1), stops.recv())
    .await
    .unwrap()
    .unwrap();
  let mut reopened = registry
    .open_with(&target, &path, Service::Task, || {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 2);
  reopened.write_all(b"new").await.unwrap();
  let mut bytes = [0; 3];
  reopened.read_exact(&mut bytes).await.unwrap();
  assert_eq!(&bytes, b"new");
  registry.shutdown();
  timeout(Duration::from_secs(1), stops.recv())
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn startup_requires_advertised_transport_and_keeps_inner_preface_bytes() {
  for supported in [false, true] {
    let component = ctl_core::component::ComponentInfo {
      build: ctl_core::component::build_info(),
      protocols: if supported {
        ctl_proto::agent_protocols()
      } else {
        Vec::new()
      },
    };
    let mut bytes = b"login banner\nctl-multiplex-startup\n".to_vec();
    bytes.extend(serde_json::to_vec(&component).unwrap());
    bytes.extend_from_slice(b"\nremaining protocol bytes");
    let mut reader = bytes.as_slice();
    let result = inspect_capability(&mut reader).await;
    if supported {
      result.unwrap();
      assert_eq!(reader, b"remaining protocol bytes");
    } else {
      let error = result.unwrap_err();
      assert_eq!(error.kind(), io::ErrorKind::Unsupported);
      assert!(error.to_string().contains("Update the remote components"));
    }
  }
  let bytes = vec![b'x'; MAX_COMPONENT_BYTES + 1];
  assert_eq!(
    inspect_capability(&mut bytes.as_slice())
      .await
      .unwrap_err()
      .kind(),
    io::ErrorKind::InvalidData
  );
}

#[tokio::test]
async fn startup_keeps_install_and_upgrade_failures_distinct_from_ssh_failures() {
  for (bytes, expected) in [
    (MISSING_AGENT_MARKER.to_vec(), "ctl_agent_not_found"),
    (
      [STARTUP_MARKER, UNSUPPORTED_AGENT_MARKER].concat(),
      "ctl_agent_update_required",
    ),
    (Vec::new(), "ssh_transport_failed"),
  ] {
    let error = inspect_capability(&mut bytes.as_slice()).await.unwrap_err();
    assert_eq!(RequestError::AgentTransport(error).code(), expected);
  }
  // A missing local control socket requires reconnecting, not installing an agent.
  assert_eq!(
    RequestError::AgentTransport(io::ErrorKind::NotFound.into()).code(),
    "ssh_transport_failed"
  );
}

#[tokio::test]
async fn startup_preserves_real_ssh_diagnostics_without_install_or_upgrade_markers() {
  let diagnostics = "mux_client_request_session: session request failed: Session open refused by peer\nConnection closed by UNKNOWN port 65535\n";
  for stdout in ["", "login banner\n", "ctl-multiplex-startup\n"] {
    let mut command = Command::new("sh");
    command
      .args([
        "-c",
        "printf '%s' \"$1\"; printf '%s' \"$2\" >&2; exit 255",
        "fixture",
        stdout,
        diagnostics,
      ])
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let error = start(command).await.err().expect("failed SSH child");
    assert!(!agent_is_missing(&error));
    assert!(error.to_string().contains("Session open refused by peer"));
    assert!(error.to_string().contains("UNKNOWN port 65535"));
    assert_eq!(
      RequestError::AgentTransport(error).code(),
      "ssh_transport_failed"
    );
  }
}

#[test]
fn service_handoff_requires_the_new_local_contract() {
  let request = ctl_ipc::ClientMessage::OpenService {
    target: serde_json::from_value(serde_json::json!({"destination":"fixture.invalid"})).unwrap(),
    control_path: PathBuf::from("/fixture/control"),
    service: Service::Ctmux,
  };
  for version in ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS {
    assert_eq!(
      super::super::validate_request_contract(&request, *version).is_ok(),
      ctl_ipc::service_streams_supported(*version)
    );
  }
}

struct BrokenService(bool);

impl AsyncRead for BrokenService {
  fn poll_read(
    mut self: Pin<&mut Self>,
    _: &mut Context<'_>,
    bytes: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    if self.0 {
      Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    } else {
      self.0 = true;
      bytes.put_slice(b"inner service bytes");
      Poll::Ready(Ok(()))
    }
  }
}

impl AsyncWrite for BrokenService {
  fn poll_write(
    self: Pin<&mut Self>,
    _: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    Poll::Ready(Ok(bytes.len()))
  }

  fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }

  fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
    Poll::Ready(Ok(()))
  }
}

#[tokio::test]
async fn relay_reset_after_handoff_never_inserts_a_broker_error_frame() {
  let (mut client, mut server) = tokio::net::UnixStream::pair().unwrap();
  let relay = tokio::spawn(async move {
    let lifecycle = super::super::target_lifecycle::TargetLifecycle::default();
    let error = relay_service(
      &mut server,
      &mut BrokenService(false),
      &mut lifecycle.attempt(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, RequestError::ServiceStreamClosed(_)));
    super::super::respond_to_request_error(&mut server, &error).await;
  });
  assert!(matches!(
    ctl_ipc::read_frame(&mut client).await.unwrap(),
    Some(ctl_ipc::ServerMessage::ServiceOpened)
  ));
  let mut bytes = Vec::new();
  timeout(Duration::from_secs(2), client.read_to_end(&mut bytes))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(bytes, b"inner service bytes");
  relay.await.unwrap();
}

#[tokio::test]
async fn disconnecting_a_target_closes_its_raw_relay_without_broker_frames() {
  let lifecycle = Arc::new(super::super::target_lifecycle::TargetLifecycle::default());
  let mut clients = Vec::new();
  let mut services = JoinSet::new();
  for index in 0..2 {
    let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
    let (mut remote, peer) = tokio::io::duplex(1024);
    let independent = super::super::target_lifecycle::TargetLifecycle::default();
    let mut attempt = if index == 0 {
      lifecycle.attempt()
    } else {
      independent.attempt()
    };
    services.spawn(async move {
      let result = relay_service(&mut server, &mut remote, &mut attempt).await;
      if let Err(error) = &result {
        super::super::respond_to_request_error(&mut server, error).await;
      }
    });
    // The echo peer belongs to this service only.
    services.spawn(async move {
      let (mut reader, mut writer) = tokio::io::split(peer);
      let _ = tokio::io::copy(&mut reader, &mut writer).await;
    });
    clients.push((client, independent));
  }
  for (client, _) in &mut clients {
    assert!(matches!(
      ctl_ipc::read_frame(client).await.unwrap(),
      Some(ctl_ipc::ServerMessage::ServiceOpened)
    ));
  }
  lifecycle.pause();
  let mut ended = Vec::new();
  timeout(Duration::from_secs(2), clients[0].0.read_to_end(&mut ended))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(ended, [] as [u8; 0]);
  clients[1].0.write_all(b"alive").await.unwrap();
  let mut reply = [0; 5];
  timeout(Duration::from_secs(2), clients[1].0.read_exact(&mut reply))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(&reply, b"alive");
  drop(clients);
  timeout(Duration::from_secs(2), async {
    while let Some(result) = services.join_next().await {
      result.unwrap();
    }
  })
  .await
  .unwrap();
}
