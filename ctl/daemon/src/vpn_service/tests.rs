use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{sleep, timeout};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(2);

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
}

impl FakeLease {
  fn new(probe: &Arc<Probe>) -> (Self, oneshot::Sender<()>) {
    let (exit, receiver) = oneshot::channel();
    (
      Self {
        probe: Arc::clone(probe),
        exit: receiver,
      },
      exit,
    )
  }
}

impl Lease for FakeLease {
  fn status(&self) -> VpnStatus {
    VpnStatus {
      running: true,
      endpoint: Some("socks5h://127.0.0.1:54321".to_owned()),
      container_name: Some("test-vpn".to_owned()),
      connection_id: None,
      state: VpnState::Connected,
    }
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

struct Harness {
  service: VpnService,
  owner: VpnOwner,
  starts: mpsc::UnboundedReceiver<oneshot::Sender<io::Result<FakeLease>>>,
  probe: Arc<Probe>,
}

impl Harness {
  fn new() -> Self {
    let probe = Arc::new(Probe::default());
    let factory_probe = Arc::clone(&probe);
    let (started, starts) = mpsc::unbounded_channel();
    let (service, owner) = spawn_with(move |_| {
      let (ready, result) = oneshot::channel();
      let guard = StartupGuard(Arc::clone(&factory_probe));
      let _ = started.send(ready);
      async move {
        let _guard = guard;
        result
          .await
          .unwrap_or_else(|_| Err(io::Error::other("fake startup ended")))
      }
    });
    Self {
      service,
      owner,
      starts,
      probe,
    }
  }

  async fn begin_start(
    &mut self,
  ) -> (
    JoinHandle<Result<VpnStatus, String>>,
    oneshot::Sender<io::Result<FakeLease>>,
  ) {
    let service = self.service.clone();
    let request = tokio::spawn(async move { service.start(std::env::temp_dir()).await });
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
  let mut harness = Harness::new();
  let (request, ready) = harness.begin_start().await;
  let status = harness.service.status().await.unwrap();
  assert!(!status.running);
  assert_eq!(status.state, VpnState::Starting);
  assert_eq!(status.connection_id, None);
  assert!(
    harness
      .service
      .start(std::env::current_exe().unwrap())
      .await
      .unwrap_err()
      .contains("already starting")
  );
  assert!(!harness.service.stop().await.unwrap().running);
  assert!(request.await.unwrap().unwrap_err().contains("cancelled"));
  assert!(ready.is_closed());
  assert_eq!(harness.probe.startup_drops.load(Ordering::SeqCst), 1);
  harness.owner.shutdown().await;
}

#[tokio::test]
async fn same_config_reuses_the_lease_and_another_config_requires_stop() {
  let mut harness = Harness::new();
  let _exit = harness.start_ready().await;
  let first = harness.service.status().await.unwrap();
  let duplicate = harness
    .service
    .start(std::env::temp_dir().join("."))
    .await
    .unwrap();
  assert_eq!(first, duplicate);
  let error = harness
    .service
    .start(std::env::current_exe().unwrap())
    .await
    .unwrap_err();
  assert!(error.contains("stop it first"));
  assert!(harness.starts.try_recv().is_err());
  assert!(!harness.service.stop().await.unwrap().running);
  assert_eq!(harness.probe.shutdowns.load(Ordering::SeqCst), 1);
  assert_eq!(harness.probe.lease_drops.load(Ordering::SeqCst), 1);
  assert!(!harness.service.status().await.unwrap().running);
  harness.owner.shutdown().await;
}

#[tokio::test]
async fn failed_start_can_be_retried_without_restarting_the_service() {
  let mut harness = Harness::new();
  let (request, ready) = harness.begin_start().await;
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
  assert!(!harness.service.status().await.unwrap().running);
  let _exit = harness.start_ready().await;
  harness.owner.shutdown().await;
  assert_eq!(harness.probe.shutdowns.load(Ordering::SeqCst), 1);
  assert!(harness.service.status().await.is_err());
  harness.owner.shutdown().await;
}

#[tokio::test]
async fn unexpected_exit_clears_status_and_allows_another_start() {
  let mut harness = Harness::new();
  let exit = harness.start_ready().await;
  exit.send(()).unwrap();
  wait_for_count(&harness.probe.lease_drops, 1).await;
  assert!(!harness.service.status().await.unwrap().running);
  let _exit = harness.start_ready().await;
  harness.owner.shutdown().await;
  assert_eq!(harness.probe.shutdowns.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn dropping_owner_cancels_pending_start_even_with_request_handles_alive() {
  let mut harness = Harness::new();
  let (request, ready) = harness.begin_start().await;
  drop(harness.owner);
  wait_for_count(&harness.probe.startup_drops, 1).await;
  assert!(ready.is_closed());
  assert!(request.await.unwrap().is_err());
  assert!(harness.service.status().await.is_err());
}

#[tokio::test]
async fn owner_shutdown_cancels_pending_start_and_answers_its_request() {
  let mut harness = Harness::new();
  let (request, ready) = harness.begin_start().await;
  harness.owner.shutdown().await;
  assert!(ready.is_closed());
  assert!(request.await.unwrap().unwrap_err().contains("cancelled"));
  assert_eq!(harness.probe.startup_drops.load(Ordering::SeqCst), 1);
  assert!(harness.service.status().await.is_err());
}

#[tokio::test]
async fn dropping_owner_releases_the_active_container_lease() {
  let mut harness = Harness::new();
  let _exit = harness.start_ready().await;
  drop(harness.owner);
  wait_for_count(&harness.probe.lease_drops, 1).await;
  assert!(harness.service.status().await.is_err());
}

#[tokio::test]
async fn closing_all_request_handles_shuts_down_the_container() {
  let mut harness = Harness::new();
  let _exit = harness.start_ready().await;
  drop(harness.service);
  wait_for_count(&harness.probe.shutdowns, 1).await;
  wait_for_count(&harness.probe.lease_drops, 1).await;
  harness.owner.shutdown().await;
}

fn saved_connection() -> VpnConnection {
  VpnConnection {
    connection_id: "profile-test".into(),
    name: "Test VPN".into(),
    url: "vpn.example.test".into(),
    username: "test-user".into(),
    password: Zeroizing::new("test-password".into()),
    auth_method: None,
    target_ip: None,
  }
}

#[tokio::test]
async fn saved_connections_report_phases_and_require_stop_before_config_changes() {
  let mut harness = Harness::new();
  let service = harness.service.clone();
  let request = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let ready = timeout(TEST_TIMEOUT, harness.starts.recv())
    .await
    .unwrap()
    .unwrap();
  let status = harness.service.status().await.unwrap();
  assert_eq!(status.state, VpnState::Starting);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert!(!status.running);

  let service = harness.service.clone();
  let duplicate = tokio::spawn(async move { service.start_connection(saved_connection()).await });
  let (lease, _exit) = FakeLease::new(&harness.probe);
  assert!(ready.send(Ok(lease)).is_ok());
  let status = request.await.unwrap().unwrap();
  assert_eq!(status.state, VpnState::Connected);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert_eq!(duplicate.await.unwrap().unwrap(), status);
  assert!(harness.starts.try_recv().is_err());

  let mut changed = saved_connection();
  changed.password = Zeroizing::new("changed-password".into());
  let error = harness.service.start_connection(changed).await.unwrap_err();
  assert!(error.contains("stop it first"));
  assert!(!error.contains("changed-password"));

  let (release, wait) = oneshot::channel();
  *harness.probe.shutdown_gate.lock().unwrap() = Some(wait);
  let service = harness.service.clone();
  let stopping = tokio::spawn(async move { service.stop().await });
  wait_for_count(&harness.probe.shutdowns, 1).await;
  let status = harness.service.status().await.unwrap();
  assert_eq!(status.state, VpnState::Stopping);
  assert_eq!(status.connection_id.as_deref(), Some("profile-test"));
  assert!(!status.running);
  assert!(status.endpoint.is_none());
  release.send(()).unwrap();
  assert_eq!(stopping.await.unwrap().unwrap(), VpnStatus::default());
  assert_eq!(
    harness.service.status().await.unwrap(),
    VpnStatus::default()
  );
  harness.owner.shutdown().await;
}
