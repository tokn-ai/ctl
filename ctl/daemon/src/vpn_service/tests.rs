use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{sleep, timeout};
use zeroize::Zeroizing;

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn distinct_immutable_containers_are_not_hidden_by_a_reused_profile_id() {
  let mut snapshot = VpnSnapshot {
    connections: vec![VpnStatus {
      vpn_id: Some("profile".into()),
      container_id: Some("first".into()),
      ..VpnStatus::default()
    }],
    ..VpnSnapshot::default()
  };
  merge_discovered(
    &mut snapshot,
    vec![VpnStatus {
      vpn_id: Some("profile".into()),
      container_id: Some("second".into()),
      ..VpnStatus::default()
    }],
  );
  assert_eq!(snapshot.connections.len(), 2);
}

#[test]
fn release_without_inventory_retains_uncertain_container_metadata() {
  let local = VpnSnapshot {
    connections: vec![VpnStatus {
      vpn_id: Some("profile".into()),
      container_id: Some("immutable-id".into()),
      endpoint: Some("socks5h://127.0.0.1:54321".into()),
      running: true,
      state: VpnState::Connected,
      locally_connected: Some(true),
      ..VpnStatus::default()
    }],
    ..VpnSnapshot::default()
  };
  let status = released_without_inventory(&local, "profile").unwrap();
  assert_eq!(status.container_id, local.connections[0].container_id);
  assert_eq!(status.endpoint, local.connections[0].endpoint);
  assert_eq!(status.state, VpnState::Connected);
  assert_eq!(status.locally_connected, Some(false));
  assert!(status.status_unavailable);
  assert!(released_without_inventory(&local, "foreign").is_err());
}

#[test]
fn shared_inventory_preserves_local_interest_and_updates_container_readiness() {
  let local = VpnStatus {
    vpn_id: Some("profile".into()),
    container_id: Some("immutable-id".into()),
    locally_connected: Some(true),
    running: true,
    state: VpnState::Connected,
    endpoint: Some("socks5h://127.0.0.1:54321".into()),
    ..VpnStatus::default()
  };
  let mut snapshot = VpnSnapshot {
    connections: vec![local.clone()],
    ..VpnSnapshot::default()
  };
  let observed = VpnStatus {
    state: VpnState::Starting,
    running: false,
    endpoint: None,
    locally_connected: Some(false),
    ..local
  };
  merge_discovered(&mut snapshot, vec![observed]);
  assert_eq!(snapshot.connections.len(), 1);
  assert_eq!(snapshot.connections[0].locally_connected, Some(true));
  assert_eq!(snapshot.connections[0].connection_id, None);
  assert!(!snapshot.connections[0].running);
  assert_eq!(snapshot.connections[0].endpoint, None);
}

#[test]
fn discovered_containers_remain_visible_after_local_interest_is_released() {
  let mut snapshot = VpnSnapshot::default();
  let foreign = VpnStatus {
    vpn_id: Some("profile".into()),
    container_id: Some("immutable-id".into()),
    locally_connected: Some(false),
    shared_container: true,
    running: true,
    state: VpnState::Connected,
    ..VpnStatus::default()
  };
  merge_discovered(&mut snapshot, vec![foreign.clone()]);
  merge_discovered(&mut snapshot, vec![foreign]);
  assert_eq!(snapshot.connections.len(), 1);
  assert_eq!(snapshot.connections[0].locally_connected, Some(false));
  assert_eq!(snapshot.connections[0].state, VpnState::Connected);
}

#[derive(Default)]
struct Probe {
  startup_drops: AtomicUsize,
  shutdowns: AtomicUsize,
  lease_drops: AtomicUsize,
  shutdown_gate: Mutex<Option<oneshot::Receiver<()>>>,
}

struct StartupGuard(Arc<Probe>);

impl Drop for StartupGuard {
  fn drop(&mut self) {
    self.0.startup_drops.fetch_add(1, Ordering::SeqCst);
  }
}

struct FakeLease {
  probe: Arc<Probe>,
  exit: oneshot::Receiver<()>,
  status: VpnStatus,
}

impl FakeLease {
  fn new(probe: &Arc<Probe>) -> (Self, oneshot::Sender<()>) {
    let (exit, receiver) = oneshot::channel();
    (
      Self {
        probe: Arc::clone(probe),
        exit: receiver,
        status: VpnStatus {
          running: true,
          endpoint: Some("socks5h://127.0.0.1:54321".to_owned()),
          container_name: Some("test-vpn".to_owned()),
          state: VpnState::Connected,
          ..VpnStatus::default()
        },
      },
      exit,
    )
  }
}

impl Lease for FakeLease {
  fn status(&self) -> VpnStatus {
    self.status.clone()
  }

  async fn exited(&mut self) -> io::Result<()> {
    let _ = (&mut self.exit).await;
    Ok(())
  }

  async fn shutdown(&mut self) {
    self.probe.shutdowns.fetch_add(1, Ordering::SeqCst);
    let waiting = self.probe.shutdown_gate.lock().unwrap().take();
    if let Some(waiting) = waiting {
      let _ = waiting.await;
    }
  }
}

impl Drop for FakeLease {
  fn drop(&mut self) {
    self.probe.lease_drops.fetch_add(1, Ordering::SeqCst);
  }
}

struct TestFixture {
  service: VpnService,
  owner: VpnOwner,
  starts: mpsc::UnboundedReceiver<oneshot::Sender<io::Result<FakeLease>>>,
  probe: Arc<Probe>,
  env_file: TestEnvFile,
  other_env_file: TestEnvFile,
}

impl TestFixture {
  fn new() -> Self {
    let env_file = test_env_file();
    let other_env_file = test_env_file();
    let probe = Arc::new(Probe::default());
    let factory_probe = Arc::clone(&probe);
    let (started, starts) = mpsc::unbounded_channel();
    let (service, owner) = spawn_with(move |config| {
      let (ready, result) = oneshot::channel();
      let guard = StartupGuard(Arc::clone(&factory_probe));
      let _ = started.send(ready);
      let cancellation = startup_cancellation(config);
      async move {
        let _guard = guard;
        tokio::select! {
          result = result => result.unwrap_or_else(|_| Err(io::Error::other("fake startup ended"))),
          _ = cancellation => Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")),
        }
      }
    });
    Self {
      service,
      owner,
      starts,
      probe,
      env_file,
      other_env_file,
    }
  }

  async fn begin_start(
    &mut self,
  ) -> (
    JoinHandle<Result<VpnStatus, String>>,
    oneshot::Sender<io::Result<FakeLease>>,
  ) {
    let service = self.service.clone();
    let env_file = self.env_file.clone();
    let request = tokio::spawn(async move { service.start(env_file).await });
    let ready = timeout(TEST_TIMEOUT, self.starts.recv())
      .await
      .unwrap()
      .unwrap();
    (request, ready)
  }

  async fn start_ready(&mut self) -> oneshot::Sender<()> {
    let (request, ready) = self.begin_start().await;
    let (lease, exit) = FakeLease::new(&self.probe);
    assert!(ready.send(Ok(lease)).is_ok());
    assert!(
      timeout(TEST_TIMEOUT, request)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .running
    );
    exit
  }
}

fn startup_cancellation(config: Config) -> oneshot::Receiver<()> {
  match config {
    Config::Openconnect(config) => config.cancellation.unwrap(),
    Config::Tailscale(config) => config.cancellation.unwrap(),
  }
}

struct TestEnvFile(PathBuf);

impl std::ops::Deref for TestEnvFile {
  type Target = PathBuf;

  fn deref(&self) -> &PathBuf {
    &self.0
  }
}

impl Drop for TestEnvFile {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

fn test_env_file() -> TestEnvFile {
  let path = std::env::temp_dir().join(format!("ctld-vpn-config-{}", uuid::Uuid::new_v4()));
  std::fs::write(
    &path,
    "VPN_URL=vpn.example.test\nVPN_USERNAME=test-user\nVPN_PASSWORD=test-password\n",
  )
  .unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
  }
  TestEnvFile(path)
}

async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
  timeout(TEST_TIMEOUT, async {
    while counter.load(Ordering::SeqCst) != expected {
      sleep(Duration::from_millis(5)).await;
    }
  })
  .await
  .expect("lifecycle action did not complete");
}

#[tokio::test]
async fn stop_cancels_startup_and_finishes_the_waiting_request() {
  let mut fixture = TestFixture::new();
  let (request, ready) = fixture.begin_start().await;
  let status = fixture.service.status().await.unwrap();
  assert!(!status.running);
  assert_eq!(status.state, VpnState::Starting);
  assert_eq!(status.connection_id, None);
  assert_eq!(status.vpn_url.as_deref(), Some("https://vpn.example.test"));
  assert_eq!(status.username.as_deref(), Some("test-user"));
  assert!(!fixture.service.stop().await.unwrap().running);
  assert!(request.await.unwrap().unwrap_err().contains("cancelled"));
  assert!(ready.is_closed());
  assert_eq!(fixture.probe.startup_drops.load(Ordering::SeqCst), 1);
  fixture.owner.shutdown().await;
}

#[tokio::test]
async fn same_config_reuses_the_lease_and_keeps_its_original_metadata() {
  let mut fixture = TestFixture::new();
  let _exit = fixture.start_ready().await;
  let first = fixture.service.status().await.unwrap();
  assert_eq!(first.vpn_url.as_deref(), Some("https://vpn.example.test"));
  assert_eq!(first.username.as_deref(), Some("test-user"));
  std::fs::write(
    &*fixture.env_file,
    "VPN_URL=changed.example.test\nVPN_USERNAME=changed-user\nVPN_PASSWORD=changed-password\n",
  )
  .unwrap();
  // Polling and repeated start commands describe the existing lease, even if
  // the source settings have since been edited.
  assert_eq!(fixture.service.status().await.unwrap(), first);
  let duplicate = fixture
    .service
    .start(
      fixture
        .env_file
        .parent()
        .unwrap()
        .join(".")
        .join(fixture.env_file.file_name().unwrap()),
    )
    .await
    .unwrap();
  assert_eq!(first, duplicate);
  assert!(fixture.starts.try_recv().is_err());
  assert!(!fixture.service.stop().await.unwrap().running);
  assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 1);
  assert_eq!(fixture.probe.lease_drops.load(Ordering::SeqCst), 1);
  assert!(!fixture.service.status().await.unwrap().running);
  fixture.owner.shutdown().await;
}

#[tokio::test]
async fn registry_capacity_is_bounded_and_targeted_stop_releases_only_its_entry() {
  let mut registry = Registry::new(|config| async move {
    let _ = startup_cancellation(config).await;
    Err::<FakeLease, _>(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
  });
  let mut results = Vec::new();
  for index in 0..MAX_CONNECTIONS {
    let mut connection = saved_connection();
    connection.connection_id = format!("profile-{index}");
    let (reply, result) = oneshot::channel();
    registry.start(Source::Connection(connection), reply);
    results.push(result);
  }
  let (reply, result) = oneshot::channel();
  registry.start(Source::Connection(saved_connection()), reply);
  assert!(result.await.unwrap().unwrap_err().contains("At most 16"));
  assert_eq!(registry.snapshot().connections.len(), MAX_CONNECTIONS);
  let (reply, stopped) = oneshot::channel();
  registry.stop(Some("profile-0".into()), reply);
  let (id, event) = poll_fn(|cx| registry.poll_event(cx)).await;
  registry.event(id, event);
  assert_eq!(stopped.await.unwrap().unwrap(), VpnStatus::default());
  assert!(
    results
      .remove(0)
      .await
      .unwrap()
      .unwrap_err()
      .contains("cancelled")
  );
  assert_eq!(registry.snapshot().connections.len(), MAX_CONNECTIONS - 1);
  for result in &mut results {
    assert!(matches!(
      result.try_recv(),
      Err(oneshot::error::TryRecvError::Empty)
    ));
  }
  registry.shutdown().await;
  for result in results {
    assert!(result.await.unwrap().unwrap_err().contains("cancelled"));
  }
}

#[tokio::test]
async fn failed_start_can_be_retried_without_restarting_the_service() {
  let mut fixture = TestFixture::new();
  let (request, ready) = fixture.begin_start().await;
  assert!(
    ready
      .send(Err(io::Error::other("test authentication failed")))
      .is_ok()
  );
  assert!(
    request
      .await
      .unwrap()
      .unwrap_err()
      .contains("test authentication failed")
  );
  assert!(!fixture.service.status().await.unwrap().running);
  let _exit = fixture.start_ready().await;
  fixture.owner.shutdown().await;
  assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 1);
  assert!(fixture.service.status().await.is_err());
  fixture.owner.shutdown().await;
}

#[tokio::test]
async fn unexpected_exit_clears_status_and_allows_another_start() {
  let mut fixture = TestFixture::new();
  let exit = fixture.start_ready().await;
  exit.send(()).unwrap();
  wait_for_count(&fixture.probe.lease_drops, 1).await;
  assert!(!fixture.service.status().await.unwrap().running);
  let _exit = fixture.start_ready().await;
  fixture.owner.shutdown().await;
  assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dropping_owner_cancels_pending_start_even_with_request_handles_alive() {
  let mut fixture = TestFixture::new();
  let (request, ready) = fixture.begin_start().await;
  drop(fixture.owner);
  wait_for_count(&fixture.probe.startup_drops, 1).await;
  assert!(ready.is_closed());
  assert!(request.await.unwrap().is_err());
  assert!(fixture.service.status().await.is_err());
}

#[tokio::test]
async fn owner_shutdown_cancels_pending_start_and_answers_its_request() {
  let mut fixture = TestFixture::new();
  let (request, ready) = fixture.begin_start().await;
  fixture.owner.shutdown().await;
  assert!(ready.is_closed());
  assert!(request.await.unwrap().unwrap_err().contains("cancelled"));
  assert_eq!(fixture.probe.startup_drops.load(Ordering::SeqCst), 1);
  assert!(fixture.service.status().await.is_err());
}

#[tokio::test]
async fn dropping_owner_releases_the_active_container_lease() {
  let mut fixture = TestFixture::new();
  let _exit = fixture.start_ready().await;
  drop(fixture.owner);
  wait_for_count(&fixture.probe.lease_drops, 1).await;
  assert!(fixture.service.status().await.is_err());
}

#[tokio::test]
async fn closing_all_request_handles_shuts_down_the_container() {
  let mut fixture = TestFixture::new();
  let _exit = fixture.start_ready().await;
  drop(fixture.service);
  wait_for_count(&fixture.probe.shutdowns, 1).await;
  wait_for_count(&fixture.probe.lease_drops, 1).await;
  fixture.owner.shutdown().await;
}

fn saved_connection() -> VpnConnection {
  VpnConnection {
    connection_id: "profile-test".into(),
    name: "Test VPN".into(),
    settings: VpnSettings::Openconnect {
      url: "vpn.example.test".into(),
      username: "test-user".into(),
      password: Zeroizing::new("test-password".into()),
      auth_method: None,
      target_ip: None,
    },
  }
}

#[tokio::test]
async fn concurrent_connections_cancel_fail_and_stop_independently() {
  let mut fixture = TestFixture::new();
  let first_exit = fixture.start_ready().await;
  let first = fixture.service.status().await.unwrap();
  let first_id = first.vpn_id.clone().unwrap();

  let service = fixture.service.clone();
  let starting = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  let snapshot = fixture.service.list().await.unwrap();
  assert!(snapshot.supports_multiple);
  assert_eq!(snapshot.connections.len(), 2);
  assert!(snapshot.connections.contains(&first));
  assert!(
    fixture
      .service
      .stop()
      .await
      .unwrap_err()
      .contains("specify a VPN ID")
  );
  assert_eq!(fixture.service.list().await.unwrap(), snapshot);

  assert_eq!(
    fixture
      .service
      .stop_id("profile-test".into())
      .await
      .unwrap(),
    VpnStatus::default()
  );
  assert!(starting.await.unwrap().unwrap_err().contains("cancelled"));
  assert!(ready.is_closed());
  assert_eq!(
    fixture.service.list().await.unwrap().connections,
    vec![first.clone()]
  );

  let service = fixture.service.clone();
  let failing = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(
    ready
      .send(Err(io::Error::other("synthetic failure")))
      .is_ok()
  );
  assert!(failing.await.unwrap().is_err());
  assert_eq!(
    fixture.service.list().await.unwrap().connections,
    vec![first.clone()]
  );

  let service = fixture.service.clone();
  let starting = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  let (lease, _second_exit) = FakeLease::new(&fixture.probe);
  assert!(ready.send(Ok(lease)).is_ok());
  let second = starting.await.unwrap().unwrap();
  assert_eq!(second.vpn_id.as_deref(), Some("profile-test"));
  assert_eq!(fixture.service.list().await.unwrap().connections.len(), 2);

  assert_eq!(
    fixture.service.stop_id("not-owned".into()).await.unwrap(),
    VpnStatus::default()
  );
  assert_eq!(fixture.service.list().await.unwrap().connections.len(), 2);
  first_exit.send(()).unwrap();
  wait_for_count(&fixture.probe.shutdowns, 1).await;
  wait_for_count(&fixture.probe.lease_drops, 1).await;
  assert_eq!(
    fixture.service.list().await.unwrap().connections,
    vec![second.clone()]
  );
  assert_eq!(
    fixture.service.stop_id(first_id).await.unwrap(),
    VpnStatus::default()
  );
  assert_eq!(
    fixture.service.list().await.unwrap().connections,
    vec![second]
  );
  fixture.owner.shutdown().await;
  assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 2);
  assert_eq!(fixture.probe.lease_drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn different_env_files_start_concurrently_and_shutdown_cleans_every_lease() {
  let mut fixture = TestFixture::new();
  let (first_request, first_ready) = fixture.begin_start().await;
  let service = fixture.service.clone();
  let path = fixture.other_env_file.clone();
  let second_request = tokio::spawn(async move { service.start(path).await });
  let second_ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  let snapshot = fixture.service.list().await.unwrap();
  assert_eq!(snapshot.connections.len(), 2);
  assert!(
    snapshot
      .connections
      .iter()
      .all(|status| status.state == VpnState::Starting && status.connection_id.is_none())
  );
  assert_ne!(
    snapshot.connections[0].vpn_id,
    snapshot.connections[1].vpn_id
  );
  let (first_lease, _first_exit) = FakeLease::new(&fixture.probe);
  let (second_lease, _second_exit) = FakeLease::new(&fixture.probe);
  assert!(first_ready.send(Ok(first_lease)).is_ok());
  assert!(second_ready.send(Ok(second_lease)).is_ok());
  assert!(first_request.await.unwrap().unwrap().running);
  assert!(second_request.await.unwrap().unwrap().running);
  fixture.owner.shutdown().await;
  assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 2);
  assert_eq!(fixture.probe.lease_drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn saved_connections_report_phases_and_require_stop_before_config_changes() {
  let mut fixture = TestFixture::new();
  let service = fixture.service.clone();
  let request = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  let status = fixture.service.status().await.unwrap();
  assert_eq!(status.state, VpnState::Starting);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_url.as_deref(), Some("https://vpn.example.test"));
  assert_eq!(status.username.as_deref(), Some("test-user"));
  assert!(!status.running);

  let service = fixture.service.clone();
  let duplicate = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let (lease, _exit) = FakeLease::new(&fixture.probe);
  assert!(ready.send(Ok(lease)).is_ok());
  let status = request.await.unwrap().unwrap();
  assert_eq!(status.state, VpnState::Connected);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_url.as_deref(), Some("https://vpn.example.test"));
  assert_eq!(status.username.as_deref(), Some("test-user"));
  assert_eq!(duplicate.await.unwrap().unwrap(), status);
  assert!(fixture.starts.try_recv().is_err());

  let mut changed = saved_connection();
  let VpnSettings::Openconnect { url, .. } = &mut changed.settings else {
    unreachable!()
  };
  url.push_str("/different-login");
  let error = fixture.service.start_connection(changed).await.unwrap_err();
  assert!(error.contains("stop this connection"));
  assert!(!error.contains("different-login"));

  let mut credential_update = saved_connection();
  credential_update.name = "Renamed profile".into();
  let VpnSettings::Openconnect { password, .. } = &mut credential_update.settings else {
    unreachable!()
  };
  *password = Zeroizing::new("changed-password".into());
  assert_eq!(
    fixture
      .service
      .start_connection(credential_update)
      .await
      .unwrap(),
    status
  );
  assert!(fixture.starts.try_recv().is_err());

  let (release, wait) = oneshot::channel();
  *fixture.probe.shutdown_gate.lock().unwrap() = Some(wait);
  let service = fixture.service.clone();
  let stopping = tokio::spawn(async move { service.stop().await });
  wait_for_count(&fixture.probe.shutdowns, 1).await;
  let status = fixture.service.status().await.unwrap();
  assert_eq!(status.state, VpnState::Stopping);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_id.as_deref(), Some("profile-test"));
  assert_eq!(status.vpn_url.as_deref(), Some("https://vpn.example.test"));
  assert_eq!(status.username.as_deref(), Some("test-user"));
  assert!(!status.running);
  assert!(status.endpoint.is_none());
  assert!(
    fixture
      .service
      .start_connection(saved_connection())
      .await
      .unwrap_err()
      .contains("VPN is stopping")
  );
  release.send(()).unwrap();
  assert_eq!(stopping.await.unwrap().unwrap(), VpnStatus::default());
  assert_eq!(
    fixture.service.status().await.unwrap(),
    VpnStatus::default()
  );
  fixture.owner.shutdown().await;
}

#[tokio::test]
async fn browser_login_remains_owned_and_idempotent_until_stop_or_daemon_shutdown() {
  for explicit_stop in [false, true] {
    let mut fixture = TestFixture::new();
    let connection = VpnConnection {
      connection_id: "tailnet-test".into(),
      name: "Test tailnet".into(),
      settings: VpnSettings::Tailscale {
        hostname: None,
        accept_routes: false,
      },
    };
    let service = fixture.service.clone();
    let requested = connection.clone();
    let starting = tokio::spawn(async move { service.start_connection(requested).await });
    let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
      .await
      .unwrap()
      .unwrap();
    let preparing = fixture.service.status().await.unwrap();
    assert_eq!(preparing.provider, VpnProvider::Tailscale);
    assert_eq!(preparing.state, VpnState::Starting);
    let (mut lease, _exit) = FakeLease::new(&fixture.probe);
    lease.status = VpnStatus {
      provider: VpnProvider::Tailscale,
      state: VpnState::Starting,
      auth_url: Some("https://login.tailscale.com/a/example".into()),
      ..VpnStatus::default()
    };
    assert!(ready.send(Ok(lease)).is_ok());
    let status = starting.await.unwrap().unwrap();
    assert_eq!(status.state, VpnState::Starting);
    assert_eq!(status.provider, VpnProvider::Tailscale);
    assert!(!status.running);
    assert!(status.auth_url.is_some());
    assert_eq!(
      fixture.service.start_connection(connection).await.unwrap(),
      status
    );
    assert!(fixture.starts.try_recv().is_err());
    assert_eq!(
      fixture.service.list().await.unwrap().supported_providers,
      vec![VpnProvider::Openconnect, VpnProvider::Tailscale]
    );
    if explicit_stop {
      fixture
        .service
        .stop_id("tailnet-test".into())
        .await
        .unwrap();
    }
    fixture.owner.shutdown().await;
    assert_eq!(fixture.probe.shutdowns.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.probe.lease_drops.load(Ordering::SeqCst), 1);
  }
}

#[tokio::test]
async fn stopping_a_tailscale_start_waits_for_the_provider_to_release_its_container() {
  let (cleanup_ready, mut cleanup_started) = mpsc::unbounded_channel();
  let (cleanup_done, mut cleanup_finish) = mpsc::unbounded_channel();
  let cleanup_gate = Arc::new(Mutex::new(Some(oneshot::channel::<()>())));
  let gate = Arc::clone(&cleanup_gate);
  let (service, mut owner) = spawn_with(move |config| {
    let Config::Tailscale(mut config) = config else {
      unreachable!()
    };
    let cancellation = config.cancellation.take().unwrap();
    let (done, wait) = gate.lock().unwrap().take().unwrap();
    cleanup_done.send(done).unwrap();
    let started = cleanup_ready.clone();
    async move {
      let _ = cancellation.await;
      started.send(()).unwrap();
      let _ = wait.await;
      Err::<FakeLease, _>(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
    }
  });
  let request_service = service.clone();
  let request = tokio::spawn(async move {
    request_service
      .start_connection(VpnConnection {
        connection_id: "tailnet-test".into(),
        name: "Test tailnet".into(),
        settings: VpnSettings::Tailscale {
          hostname: None,
          accept_routes: false,
        },
      })
      .await
  });
  let done = timeout(TEST_TIMEOUT, cleanup_finish.recv())
    .await
    .unwrap()
    .unwrap();
  let stop_service = service.clone();
  let stopping = tokio::spawn(async move { stop_service.stop_id("tailnet-test".into()).await });
  timeout(TEST_TIMEOUT, cleanup_started.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(!stopping.is_finished());
  assert_eq!(service.status().await.unwrap().state, VpnState::Stopping);
  assert!(request.await.unwrap().unwrap_err().contains("cancelled"));
  done.send(()).unwrap();
  timeout(TEST_TIMEOUT, stopping)
    .await
    .unwrap()
    .unwrap()
    .unwrap();
  assert_eq!(
    service.list().await.unwrap().connections,
    Vec::<VpnStatus>::new()
  );
  owner.shutdown().await;
}

#[tokio::test]
async fn forgetting_a_preparing_or_connected_identity_is_rejected_before_engine_access() {
  let mut fixture = TestFixture::new();
  let connection = VpnConnection {
    connection_id: "tailnet-test".into(),
    name: "Test tailnet".into(),
    settings: VpnSettings::Tailscale {
      hostname: None,
      accept_routes: false,
    },
  };
  let service = fixture.service.clone();
  let request = tokio::spawn(async move { service.start_connection(connection).await });
  let ready = timeout(TEST_TIMEOUT, fixture.starts.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(
    fixture
      .service
      .forget_tailscale_identity("tailnet-test".into())
      .await
      .unwrap_err()
      .contains("Stop the Tailscale connection")
  );
  let (lease, _exit) = FakeLease::new(&fixture.probe);
  assert!(ready.send(Ok(lease)).is_ok());
  request.await.unwrap().unwrap();
  assert!(
    fixture
      .service
      .forget_tailscale_identity("tailnet-test".into())
      .await
      .unwrap_err()
      .contains("Stop the Tailscale connection")
  );
  fixture.owner.shutdown().await;
}

#[tokio::test]
async fn slow_identity_cleanup_reserves_only_its_profile_and_preserves_its_result() {
  for cleanup_result in [Ok(()), Err("synthetic cleanup failure".to_owned())] {
    let probe = Arc::new(Probe::default());
    let start_probe = Arc::clone(&probe);
    let exits = Arc::new(Mutex::new(Vec::new()));
    let start_exits = Arc::clone(&exits);
    let (started, mut cleanup_started) = mpsc::unbounded_channel();
    let (service, mut owner) = spawn_with_forget(
      move |_| {
        let (lease, exit) = FakeLease::new(&start_probe);
        start_exits.lock().unwrap().push(exit);
        std::future::ready(Ok(lease))
      },
      move |_| {
        let (release, wait) = oneshot::channel();
        started.send(release).unwrap();
        async move { wait.await.unwrap() }
      },
    );
    service.start_connection(saved_connection()).await.unwrap();
    let forget_service = service.clone();
    let forgetting = tokio::spawn(async move {
      forget_service
        .forget_tailscale_identity("cancelled-draft".into())
        .await
    });
    let release = timeout(TEST_TIMEOUT, cleanup_started.recv())
      .await
      .unwrap()
      .unwrap();
    let snapshot = timeout(TEST_TIMEOUT, service.list())
      .await
      .unwrap()
      .unwrap();
    assert_eq!(snapshot.connections.len(), 2);
    assert!(snapshot.connections.iter().any(|status| {
      status.connection_id.as_deref() == Some("cancelled-draft")
        && status.state == VpnState::Stopping
    }));
    timeout(TEST_TIMEOUT, service.stop_id("profile-test".into()))
      .await
      .unwrap()
      .unwrap();
    assert_eq!(probe.shutdowns.load(Ordering::SeqCst), 1);
    let mut same_profile = saved_connection();
    same_profile.connection_id = "cancelled-draft".into();
    let error = timeout(TEST_TIMEOUT, service.start_connection(same_profile.clone()))
      .await
      .unwrap()
      .unwrap_err();
    assert!(error.contains("stopping"));
    let stop_service = service.clone();
    let stopping =
      tokio::spawn(async move { stop_service.stop_id("cancelled-draft".into()).await });
    timeout(TEST_TIMEOUT, service.list())
      .await
      .unwrap()
      .unwrap();
    assert!(!stopping.is_finished());
    release.send(cleanup_result.clone()).unwrap();
    assert_eq!(
      timeout(TEST_TIMEOUT, forgetting).await.unwrap().unwrap(),
      cleanup_result
    );
    timeout(TEST_TIMEOUT, stopping)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert_eq!(
      service.list().await.unwrap().connections,
      Vec::<VpnStatus>::new()
    );
    service.start_connection(same_profile).await.unwrap();
    owner.shutdown().await;
  }
}
