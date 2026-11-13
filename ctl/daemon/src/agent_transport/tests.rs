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

async fn assert_echo(stream: &mut Stream, payload: &[u8]) {
  stream.write_all(payload).await.unwrap();
  let mut bytes = vec![0; payload.len()];
  timeout(Duration::from_secs(2), stream.read_exact(&mut bytes))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(bytes, payload);
}

async fn assert_closed(stream: &mut Stream) {
  assert!(
    timeout(Duration::from_secs(2), stream.read_u8())
      .await
      .unwrap()
      .is_err()
  );
}

fn pool_client(registry: &Registry, path: &Path, group: Group) -> Client {
  registry.gateways.lock().unwrap()[&std::fs::canonicalize(path).unwrap()].groups[&group]
    .owned
    .lock()
    .unwrap()
    .client
    .as_ref()
    .unwrap()
    .clone()
}

async fn wait_for_owners(registry: &Registry, path: &Path, group: Group, count: usize) {
  let path = std::fs::canonicalize(path).unwrap();
  timeout(Duration::from_secs(1), async {
    loop {
      let ready = registry
        .gateways
        .lock()
        .unwrap()
        .get(&path)
        .and_then(|master| master.groups.get(&group))
        .is_some_and(|slot| slot.owned.lock().unwrap().targets.len() == count);
      if ready {
        break;
      }
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn queued_startup_uses_its_pinned_canonical_socket_after_a_symlink_is_retargeted() {
  let directory = directory();
  let first_path = directory.0.join("first");
  let second_path = directory.0.join("second");
  let alias = directory.0.join("alias");
  let _first_socket = UnixListener::bind(&first_path).unwrap();
  let _second_socket = UnixListener::bind(&second_path).unwrap();
  std::os::unix::fs::symlink(&first_path, &alias).unwrap();
  let first_canonical = std::fs::canonicalize(&first_path).unwrap();
  let first_owner = target("first.invalid");
  let next_owner = target("next.invalid");
  let registry = Arc::new(Registry::default());
  let (started, initializing) = tokio::sync::oneshot::channel();
  let pending_registry = Arc::clone(&registry);
  let pending_alias = alias.clone();
  let pending_owner = first_owner.clone();
  let pending = tokio::spawn(async move {
    pending_registry
      .open_with(&pending_owner, &pending_alias, Service::Ctmux, |_| async {
        let _ = started.send(());
        std::future::pending::<io::Result<Client>>().await
      })
      .await
  });
  initializing.await.unwrap();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(2);
  let queued_registry = Arc::clone(&registry);
  let queued_alias = alias.clone();
  let queued_owner = next_owner.clone();
  let command_target = next_owner.clone();
  let queued_starts = Arc::clone(&starts);
  let queued_stopped = stopped.clone();
  let queued = tokio::spawn(async move {
    queued_registry
      .open_with(
        &queued_owner,
        &queued_alias,
        Service::Task,
        |selected| async move {
          assert_eq!(selected, first_canonical);
          // The real SSH command receives this same pinned path, never the
          // original alias which can now point to a different live master.
          let command = command(&command_target, &selected);
          let arguments: Vec<_> = command.as_std().get_args().collect();
          assert!(
            arguments
              .windows(2)
              .any(|pair| pair == [std::ffi::OsStr::new("-S"), selected.as_os_str()])
          );
          echo_gateway(queued_starts, queued_stopped).await
        },
      )
      .await
  });
  wait_for_owners(&registry, &alias, Group::Sessions, 2).await;
  std::fs::remove_file(&alias).unwrap();
  std::os::unix::fs::symlink(&second_path, &alias).unwrap();
  registry.cancel_target(&first_owner);
  assert!(pending.await.unwrap().is_err());
  let mut pinned = timeout(Duration::from_secs(1), queued)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  assert_echo(&mut pinned, b"first physical master").await;
  let mut retargeted = registry
    .open_with(&next_owner, &alias, Service::Ctmux, |selected| async move {
      assert_eq!(selected, std::fs::canonicalize(&second_path).unwrap());
      echo_gateway(Arc::clone(&starts), stopped).await
    })
    .await
    .unwrap();
  assert_echo(&mut retargeted, b"second physical master").await;
  assert_echo(&mut pinned, b"first master is preserved").await;
  registry.shutdown();
  for _ in 0..2 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
}

#[tokio::test]
async fn disconnect_between_reservation_and_startup_cannot_restore_stale_alias_ownership() {
  for service in [Service::Ctmux, Service::Udp] {
    let directory = directory();
    let path = directory.0.join("master");
    let _socket = UnixListener::bind(&path).unwrap();
    let stale_owner = target("stale.invalid");
    let live_owner = target("live.invalid");
    let registry = Registry::default();
    let lifecycle = super::super::target_lifecycle::TargetLifecycle::default();
    let mut attempt = lifecycle.attempt();
    let guard = lifecycle.lock.lock().await;
    let reserved = registry.reserve(&stale_owner, &path, service).unwrap();
    let reserved_attempt = registry.reserve(&stale_owner, &path, service).unwrap();
    let (stopped, mut stops) = mpsc::channel(1);
    let mut live = registry
      .open_with(&live_owner, &path, service, |_| {
        echo_gateway(Arc::default(), stopped)
      })
      .await
      .unwrap();
    // Disconnect publishes its generation while waiting for this target's
    // lock. Its ownership cleanup must therefore see every old reservation.
    lifecycle.pause();
    drop(guard);
    registry.cancel_target(&stale_owner);
    let error = registry
      .open_reserved_with(reserved, |_| async {
        panic!("a cancelled reservation must not start SSH")
      })
      .await
      .err()
      .expect("cancelled reservation");
    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    assert!(matches!(
      attempt.run(registry.open_reserved(reserved_attempt)).await,
      Err(RequestError::HostDisconnected)
    ));
    assert_echo(&mut live, b"live alias").await;
    registry.cancel_target(&live_owner);
    assert_closed(&mut live).await;
    assert!(registry.gateways.lock().unwrap().is_empty());
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
}

#[tokio::test]
async fn groups_share_initialization_across_aliases_and_replace_together() {
  let directory = directory();
  let path = directory.0.join("master");
  let old_socket = UnixListener::bind(&path).unwrap();
  let target: SshTarget =
    serde_json::from_value(serde_json::json!({"destination":"fixture.invalid"})).unwrap();
  let registry = Arc::new(Registry::default());
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(3);
  let mut opens = JoinSet::new();
  for index in 0_u8..64 {
    let registry = Arc::clone(&registry);
    let mut target = target.clone();
    target.destination = format!("fixture-{}.invalid", index / 2);
    let path = path.clone();
    let starts = Arc::clone(&starts);
    let stopped = stopped.clone();
    opens.spawn(async move {
      let service = if index == 0 {
        Service::Task
      } else if index % 2 == 0 {
        Service::Ctmux
      } else {
        Service::Udp
      };
      let mut stream = registry
        .open_with(&target, &path, service, |_| echo_gateway(starts, stopped))
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
  assert_eq!(starts.load(Ordering::Relaxed), 2);
  let mut first_alias = target.clone();
  first_alias.destination = "fixture-0.invalid".into();
  registry.cancel_target(&first_alias);
  // Each alias owns both classes. Disconnecting one leaves both physical
  // agent channels available to the aliases that still share this socket.
  for stream in &mut streams {
    assert_echo(stream, b"sibling").await;
  }

  // Keep the original inode alive while replacing its pathname, as when a
  // configured master is replaced without waiting for the prior SSH to exit.
  std::fs::remove_file(&path).unwrap();
  let replacement_socket = UnixListener::bind(&path).unwrap();
  let mut replacement = registry
    .open_with(&target, &path, Service::Task, |_| {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 3);
  for _ in 0..2 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
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
async fn a_blocked_forwarding_initializer_does_not_block_sessions_on_the_same_master() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Arc::new(Registry::default());
  let (started, initializing) = tokio::sync::oneshot::channel();
  let pending_registry = Arc::clone(&registry);
  let pending_path = path.clone();
  let pending_owner = owner.clone();
  let pending = tokio::spawn(async move {
    pending_registry
      .open_with(&pending_owner, &pending_path, Service::Udp, |_| async {
        let _ = started.send(());
        std::future::pending::<io::Result<Client>>().await
      })
      .await
  });
  initializing.await.unwrap();
  let (stopped, mut stops) = mpsc::channel(1);
  let mut session = timeout(
    Duration::from_secs(1),
    registry.open_with(&owner, &path, Service::Task, |_| {
      echo_gateway(Arc::default(), stopped)
    }),
  )
  .await
  .expect("sessions have an independent initialization lock")
  .unwrap();
  assert_echo(&mut session, b"responsive").await;
  assert!(!pending.is_finished());
  // One target owns both an active session and a pending forwarding startup.
  // Disconnect must release both without awaiting that startup.
  registry.cancel_target(&owner);
  assert_closed(&mut session).await;
  assert_eq!(
    timeout(Duration::from_secs(1), pending)
      .await
      .unwrap()
      .unwrap()
      .err()
      .expect("forwarding initializer was cancelled")
      .kind(),
    io::ErrorKind::ConnectionAborted
  );
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(registry.gateways.lock().unwrap().is_empty());
}

#[tokio::test]
async fn groups_have_independent_capacity_and_group_specific_recovery() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Registry::default();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(3);
  let mut sessions = Vec::new();
  let mut forwards = Vec::new();
  let mut files = Vec::new();
  for _ in 0..transport::MAX_STREAMS {
    sessions.push(
      registry
        .open_with(&owner, &path, Service::Ctmux, |_| {
          echo_gateway(Arc::clone(&starts), stopped.clone())
        })
        .await
        .unwrap(),
    );
  }
  // Task metadata and terminal attachments intentionally share session slots.
  let session_error = registry
    .open_with(&owner, &path, Service::Task, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .err()
    .expect("session pool capacity");
  assert_eq!(session_error.kind(), io::ErrorKind::WouldBlock);
  assert!(session_error.to_string().contains("sessions pool"));
  assert!(
    session_error
      .to_string()
      .contains("Close an unused terminal view")
  );
  assert!(!session_error.to_string().contains("Stop an unused"));
  for _ in 0..transport::MAX_STREAMS {
    forwards.push(
      registry
        .open_with(&owner, &path, Service::Udp, |_| {
          echo_gateway(Arc::clone(&starts), stopped.clone())
        })
        .await
        .unwrap(),
    );
  }
  let forwarding_error = registry
    .open_with(&owner, &path, Service::Udp, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .err()
    .expect("forwarding pool capacity");
  assert_eq!(forwarding_error.kind(), io::ErrorKind::WouldBlock);
  assert!(forwarding_error.to_string().contains("UDP forwarding pool"));
  assert!(
    forwarding_error
      .to_string()
      .contains("Stop an unused UDP or Any forwarding rule")
  );
  assert!(!forwarding_error.to_string().contains("Close an unused"));
  for _ in 0..transport::MAX_STREAMS {
    files.push(
      registry
        .open_with(&owner, &path, Service::Files, |_| {
          echo_gateway(Arc::clone(&starts), stopped.clone())
        })
        .await
        .unwrap(),
    );
  }
  let file_error = registry
    .open_with(&owner, &path, Service::Files, |_| {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .err()
    .expect("file pool capacity");
  assert_eq!(file_error.kind(), io::ErrorKind::WouldBlock);
  assert!(file_error.to_string().contains("files pool"));
  assert!(
    file_error
      .to_string()
      .contains("Cancel an unused file request")
  );
  assert_eq!(starts.load(Ordering::Relaxed), 3);
  assert_echo(&mut sessions[0], b"session").await;
  assert_echo(&mut forwards[0], b"forward").await;
  assert_echo(&mut files[0], b"file").await;
  registry.shutdown();
  for _ in 0..3 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
}

#[tokio::test]
async fn a_group_failure_preserves_and_does_not_restart_its_sibling() {
  for (failed_service, healthy_service) in [
    (Service::Ctmux, Service::Udp),
    (Service::Udp, Service::Ctmux),
    (Service::Files, Service::Ctmux),
    (Service::Ctmux, Service::Files),
    (Service::Files, Service::Udp),
    (Service::Udp, Service::Files),
  ] {
    let directory = directory();
    let path = directory.0.join("master");
    let _socket = UnixListener::bind(&path).unwrap();
    let owner = target("fixture.invalid");
    let registry = Registry::default();
    let starts = Arc::new(AtomicUsize::new(0));
    let (stopped, mut stops) = mpsc::channel(3);
    let mut failed = registry
      .open_with(&owner, &path, failed_service, |_| {
        echo_gateway(Arc::clone(&starts), stopped.clone())
      })
      .await
      .unwrap();
    let mut healthy = registry
      .open_with(&owner, &path, healthy_service, |_| {
        echo_gateway(Arc::clone(&starts), stopped.clone())
      })
      .await
      .unwrap();
    pool_client(&registry, &path, Group::for_service(failed_service)).close();
    assert_closed(&mut failed).await;
    assert_echo(&mut healthy, b"healthy").await;
    let mut replacement = registry
      .open_with(&owner, &path, failed_service, |_| {
        echo_gateway(Arc::clone(&starts), stopped)
      })
      .await
      .unwrap();
    assert_eq!(starts.load(Ordering::Relaxed), 3);
    assert_echo(&mut replacement, b"recovered").await;
    assert_echo(&mut healthy, b"still healthy").await;
    registry.shutdown();
    for _ in 0..3 {
      timeout(Duration::from_secs(2), stops.recv())
        .await
        .unwrap()
        .unwrap();
    }
  }
}

#[tokio::test]
async fn disconnect_releases_only_groups_without_remaining_alias_owners() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let first_owner = target("first.invalid");
  let second_owner = target("second.invalid");
  let registry = Registry::default();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(3);
  let first_session = registry
    .open_with(&first_owner, &path, Service::Ctmux, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let mut first_forward = registry
    .open_with(&first_owner, &path, Service::Udp, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let mut second_session = registry
    .open_with(&second_owner, &path, Service::Task, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 2);
  registry.cancel_target(&first_owner);
  assert_closed(&mut first_forward).await;
  assert_echo(&mut second_session, b"remaining alias").await;
  let mut second_forward = registry
    .open_with(&second_owner, &path, Service::Udp, |_| {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 3);
  assert_echo(&mut second_forward, b"new forwarding owner").await;
  assert_echo(&mut second_session, b"same session pool").await;
  registry.cancel_target(&second_owner);
  assert_closed(&mut second_forward).await;
  assert_closed(&mut second_session).await;
  assert!(registry.gateways.lock().unwrap().is_empty());
  for _ in 0..3 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
  drop(first_session);
}

#[tokio::test]
async fn one_groups_idle_cleanup_leaves_active_siblings_available() {
  let directory = directory();
  let path = directory.0.join("master");
  let _socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Registry::default();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(3);
  let session = registry
    .open_with(&owner, &path, Service::Ctmux, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let mut forward = registry
    .open_with(&owner, &path, Service::Udp, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let idle = pool_client(&registry, &path, Group::Sessions);
  let active = pool_client(&registry, &path, Group::Forwarding);
  drop(session);
  timeout(Duration::from_secs(6), idle.closed())
    .await
    .unwrap();
  assert!(!active.is_closed());
  assert_echo(&mut forward, b"still active").await;
  let mut reopened = registry
    .open_with(&owner, &path, Service::Task, |_| {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 3);
  assert_echo(&mut reopened, b"new session").await;
  assert_echo(&mut forward, b"same forwarding pool").await;
  registry.shutdown();
  for _ in 0..3 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
}

#[tokio::test]
async fn master_replacement_closes_active_groups_and_cancels_pending_startups() {
  let directory = directory();
  let path = directory.0.join("master");
  let old_socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Arc::new(Registry::default());
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(3);
  let mut old_session = registry
    .open_with(&owner, &path, Service::Ctmux, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let (started, initializing) = tokio::sync::oneshot::channel();
  let pending_registry = Arc::clone(&registry);
  let pending_path = path.clone();
  let pending_owner = owner.clone();
  let pending = tokio::spawn(async move {
    pending_registry
      .open_with(&pending_owner, &pending_path, Service::Udp, |_| async {
        let _ = started.send(());
        std::future::pending::<io::Result<Client>>().await
      })
      .await
  });
  initializing.await.unwrap();
  std::fs::remove_file(&path).unwrap();
  let replacement_socket = UnixListener::bind(&path).unwrap();
  let mut new_session = registry
    .open_with(&owner, &path, Service::Task, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  assert_closed(&mut old_session).await;
  assert_eq!(
    timeout(Duration::from_secs(1), pending)
      .await
      .unwrap()
      .unwrap()
      .err()
      .expect("old forwarding startup was cancelled")
      .kind(),
    io::ErrorKind::ConnectionAborted
  );
  let mut new_forward = registry
    .open_with(&owner, &path, Service::Udp, |_| {
      echo_gateway(Arc::clone(&starts), stopped)
    })
    .await
    .unwrap();
  assert_eq!(starts.load(Ordering::Relaxed), 3);
  assert_echo(&mut new_session, b"new session").await;
  assert_echo(&mut new_forward, b"new forward").await;
  registry.shutdown();
  for _ in 0..3 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
  drop((old_socket, replacement_socket));
}

#[tokio::test]
async fn master_replacement_cancels_both_groups_before_either_initializer_is_ready() {
  let directory = directory();
  let path = directory.0.join("master");
  let old_socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Arc::new(Registry::default());
  let (started, mut initializing) = mpsc::channel(2);
  let mut pending = JoinSet::new();
  for service in [Service::Ctmux, Service::Udp] {
    let registry = Arc::clone(&registry);
    let path = path.clone();
    let owner = owner.clone();
    let started = started.clone();
    pending.spawn(async move {
      registry
        .open_with(&owner, &path, service, |_| async {
          started.send(()).await.unwrap();
          std::future::pending::<io::Result<Client>>().await
        })
        .await
    });
  }
  for _ in 0..2 {
    initializing.recv().await.unwrap();
  }
  std::fs::remove_file(&path).unwrap();
  let replacement_socket = UnixListener::bind(&path).unwrap();
  let (stopped, mut stops) = mpsc::channel(1);
  let mut replacement = registry
    .open_with(&owner, &path, Service::Task, |_| {
      echo_gateway(Arc::default(), stopped)
    })
    .await
    .unwrap();
  timeout(Duration::from_secs(1), async {
    while let Some(result) = pending.join_next().await {
      assert_eq!(
        result.unwrap().err().expect("old initializer").kind(),
        io::ErrorKind::ConnectionAborted
      );
    }
  })
  .await
  .unwrap();
  assert_echo(&mut replacement, b"new master").await;
  registry.shutdown();
  timeout(Duration::from_secs(2), stops.recv())
    .await
    .unwrap()
    .unwrap();
  drop((old_socket, replacement_socket));
}

#[tokio::test]
async fn a_startup_observing_master_replacement_cannot_publish_either_old_group() {
  let directory = directory();
  let path = directory.0.join("master");
  let old_socket = UnixListener::bind(&path).unwrap();
  let owner = target("fixture.invalid");
  let registry = Registry::default();
  let starts = Arc::new(AtomicUsize::new(0));
  let (stopped, mut stops) = mpsc::channel(2);
  let mut session = registry
    .open_with(&owner, &path, Service::Ctmux, |_| {
      echo_gateway(Arc::clone(&starts), stopped.clone())
    })
    .await
    .unwrap();
  let replacement = std::sync::Mutex::new(None);
  let error = registry
    .open_with(&owner, &path, Service::Udp, |_| async {
      let client = echo_gateway(Arc::clone(&starts), stopped).await?;
      std::fs::remove_file(&path).unwrap();
      *replacement.lock().unwrap() = Some(UnixListener::bind(&path).unwrap());
      Ok(client)
    })
    .await
    .err()
    .expect("the physical master changed during startup");
  assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
  assert_closed(&mut session).await;
  assert!(registry.gateways.lock().unwrap().is_empty());
  for _ in 0..2 {
    timeout(Duration::from_secs(2), stops.recv())
      .await
      .unwrap()
      .unwrap();
  }
  drop((old_socket, replacement));
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
    .open_with(&ready_target, &ready_path, Service::Ctmux, |_| {
      echo_gateway(Arc::default(), stopped)
    })
    .await
    .unwrap();
  let (started, initializing) = tokio::sync::oneshot::channel();
  let pending_registry = Arc::clone(&registry);
  let pending = tokio::spawn(async move {
    pending_registry
      .open_with(&pending_target, &pending_path, Service::Task, |_| async {
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
      .open_with(&first_owner, &first_path, Service::Ctmux, |_| async {
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
      .open_with(&next_owner, &next_path, Service::Task, |_| {
        echo_gateway(Arc::default(), stopped)
      })
      .await
  });
  wait_for_owners(&registry, &path, Group::Sessions, 2).await;
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
    .open_with(&target, &path, Service::Ctmux, |_| async {
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
    .open_with(&target, &path, Service::Task, |_| {
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

#[tokio::test]
async fn closing_a_file_request_propagates_eof_through_the_broker_without_closing_siblings() {
  timeout(Duration::from_secs(3), async {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (events, mut received) = mpsc::channel(2);
    let server = tokio::spawn(transport::serve(server_io, move |service, mut stream| {
      let events = events.clone();
      async move {
        if service == Service::Files {
          stream.read_u8().await?;
          events.send(()).await.unwrap();
          let mut byte = [0];
          assert_eq!(
            stream.read(&mut byte).await?,
            0,
            "broker must forward client closure"
          );
          events.send(()).await.unwrap();
          stream.shutdown().await
        } else {
          let (mut reader, mut writer) = tokio::io::split(stream);
          tokio::io::copy(&mut reader, &mut writer).await?;
          writer.shutdown().await
        }
      }
    }));
    let transport = transport::connect(client_io).await.unwrap();
    let mut files = transport.open(Service::Files).await.unwrap();
    let (mut client, mut broker) = ctl_ipc::Stream::pair().unwrap();
    let relay = tokio::spawn(async move {
      let lifecycle = super::super::target_lifecycle::TargetLifecycle::default();
      relay_service(&mut broker, &mut files, &mut lifecycle.attempt()).await
    });
    assert!(matches!(
      ctl_ipc::read_frame(&mut client).await.unwrap(),
      Some(ctl_ipc::ServerMessage::ServiceOpened)
    ));
    client.write_u8(1).await.unwrap();
    received.recv().await.unwrap();
    let mut sibling = transport.open(Service::Ctmux).await.unwrap();
    assert_echo(&mut sibling, b"before cancellation").await;
    drop(client);
    received.recv().await.unwrap();
    assert!(matches!(
      relay.await.unwrap(),
      Ok(()) | Err(RequestError::ServiceStreamClosed(_))
    ));
    assert_echo(&mut sibling, b"after cancellation").await;
    transport.close();
    server.await.unwrap().unwrap();
  })
  .await
  .unwrap();
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
