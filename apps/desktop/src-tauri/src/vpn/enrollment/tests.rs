use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::sync::Notify;
use tokio::time::{Duration, timeout};

use super::*;

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("rmux-enrollment-{}", uuid::Uuid::new_v4())))
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[derive(Default)]
struct FakeRuntime {
  snapshot: Mutex<VpnSnapshot>,
  started: Mutex<Option<VpnConnection>>,
  start_count: AtomicUsize,
  stop_count: AtomicUsize,
  forget_count: AtomicUsize,
  fail_support: AtomicBool,
  fail_forget: AtomicBool,
  retain_on_stop: AtomicBool,
  hold_support: AtomicBool,
  support_entered: Notify,
  release_support: Notify,
}
impl Runtime for FakeRuntime {
  fn supported(&self) -> RuntimeFuture<'_, ()> {
    Box::pin(async move {
      self.support_entered.notify_one();
      if self.hold_support.load(Ordering::SeqCst) {
        self.release_support.notified().await;
      }
      if self.fail_support.load(Ordering::SeqCst) {
        return Err(CommandErrorDto::new(
          "vpn_provider_unsupported",
          "Update ctld to sign in.",
        ));
      }
      Ok(())
    })
  }
  fn start(&self, connection: VpnConnection) -> RuntimeFuture<'_, VpnStatus> {
    Box::pin(async move {
      self.start_count.fetch_add(1, Ordering::SeqCst);
      let status = VpnStatus {
        provider: VpnProvider::Tailscale,
        vpn_id: Some(connection.connection_id.clone()),
        connection_id: Some(connection.connection_id.clone()),
        state: VpnState::Starting,
        auth_url: Some("https://login.tailscale.com/a/testtoken".into()),
        ..VpnStatus::default()
      };
      *lock(&self.started) = Some(connection);
      lock(&self.snapshot).connections = vec![status.clone()];
      Ok(status)
    })
  }
  fn list(&self) -> RuntimeFuture<'_, VpnSnapshot> {
    Box::pin(async { Ok(lock(&self.snapshot).clone()) })
  }
  fn stop(&self, _: &str) -> RuntimeFuture<'_, VpnStatus> {
    Box::pin(async {
      self.stop_count.fetch_add(1, Ordering::SeqCst);
      if self.retain_on_stop.load(Ordering::SeqCst) {
        let mut snapshot = lock(&self.snapshot);
        if let Some(status) = snapshot.connections.first_mut() {
          status.shared_container = true;
          status.locally_connected = Some(false);
          return Ok(status.clone());
        }
      }
      lock(&self.snapshot).connections.clear();
      Ok(VpnStatus::default())
    })
  }
  fn forget(&self, _: &str) -> RuntimeFuture<'_, ()> {
    Box::pin(async {
      self.forget_count.fetch_add(1, Ordering::SeqCst);
      if self.fail_forget.load(Ordering::SeqCst) {
        return Err(CommandErrorDto::new("vpn_cleanup_failed", "Retry cleanup."));
      }
      Ok(())
    })
  }
}

fn begin(
  registry: &Enrollments,
  fixture: &Fixture,
  runtime: &Arc<FakeRuntime>,
) -> VpnEnrollmentSnapshot {
  registry
    .begin_with(
      fixture.0.clone(),
      "window-one".into(),
      BeginVpnEnrollmentRequest {
        name: "Team".into(),
        hostname: None,
        accept_routes: true,
      },
      runtime.clone(),
    )
    .unwrap()
}

async fn started(registry: &Enrollments, id: &str) {
  let entry = registry.get("window-one", id).unwrap();
  timeout(Duration::from_secs(3), async {
    while lock(&entry.state).starting {
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
}

fn authenticate(runtime: &FakeRuntime) {
  let mut snapshot = lock(&runtime.snapshot);
  let status = &mut snapshot.connections[0];
  status.state = VpnState::Connected;
  status.running = true;
  status.auth_url = None;
  status.endpoint = Some("socks5h://127.0.0.1:49152".into());
}

#[tokio::test]
async fn begin_is_immediate_window_scoped_and_does_not_save_before_authentication() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  runtime.hold_support.store(true, Ordering::SeqCst);
  let draft = begin(&registry, &fixture, &runtime);
  assert_eq!(draft.status.state, VpnState::Starting);
  assert!(draft.status.message.is_some());
  assert!(!fixture.0.join("vpns.json").exists());
  assert!(
    registry
      .status("another-window", &draft.enrollment_id)
      .await
      .is_err()
  );
  runtime.release_support.notify_one();
  started(&registry, &draft.enrollment_id).await;
  let pending = registry
    .status("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert!(pending.status.auth_url.is_some());
  assert_eq!(
    registry
      .save("window-one", &draft.enrollment_id, None)
      .await
      .unwrap_err()
      .code,
    "vpn_sign_in_required"
  );
  assert!(!fixture.0.join("vpns.json").exists());
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn authenticated_save_keeps_exact_runtime_and_late_cancel_preserves_it() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  let saved = registry
    .save("window-one", &draft.enrollment_id, None)
    .await
    .unwrap();
  let stored = Repository::new(fixture.0.clone())
    .connection(&draft.connection_id)
    .unwrap();
  assert!(Some(&stored) == lock(&runtime.started).as_ref());
  let repeated = registry
    .save("window-one", &draft.enrollment_id, None)
    .await
    .unwrap();
  assert_eq!(saved, repeated);
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(runtime.start_count.load(Ordering::SeqCst), 1);
  assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  assert!(lock(&runtime.snapshot).connections[0].running);
}

#[tokio::test]
async fn stale_revision_preserves_enrollment_for_retry_and_never_overwrites_profiles() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  let repository = Repository::new(fixture.0.clone());
  let other = VpnConnection {
    connection_id: "other-profile".into(),
    name: "Other".into(),
    settings: VpnSettings::Tailscale {
      hostname: None,
      accept_routes: false,
    },
  };
  let other_saved = repository.save_enrollment(None, other.clone()).unwrap();
  assert_eq!(
    registry
      .save("window-one", &draft.enrollment_id, None)
      .await
      .unwrap_err()
      .code,
    "vpn_connections_conflict"
  );
  assert!(!repository.contains(&draft.connection_id).unwrap());
  let saved = registry
    .save("window-one", &draft.enrollment_id, other_saved.revision)
    .await
    .unwrap();
  assert_eq!(saved.connections.len(), 2);
  assert!(repository.connection("other-profile").unwrap() == other);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancel_during_preflight_never_dispatches_or_calls_runtime_cleanup() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  runtime.hold_support.store(true, Ordering::SeqCst);
  let draft = begin(&registry, &fixture, &runtime);
  runtime.support_entered.notified().await;
  let mut cancelling = Box::pin(registry.cancel("window-one", &draft.enrollment_id));
  let entry = registry.get("window-one", &draft.enrollment_id).unwrap();
  tokio::select! {
    result = &mut cancelling => panic!("cancel returned before preflight settled: {}", result.is_ok()),
    () = async {
      while lock(&entry.state).lifecycle != Lifecycle::Discarding { tokio::task::yield_now().await; }
    } => {},
  }
  runtime.release_support.notify_one();
  cancelling.await.unwrap();
  assert_eq!(runtime.start_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  assert!(registry.get("window-one", &draft.enrollment_id).is_err());
}

#[tokio::test]
async fn failed_identity_cleanup_keeps_the_enrollment_available_for_retry() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  runtime.fail_forget.store(true, Ordering::SeqCst);
  assert_eq!(
    registry
      .cancel("window-one", &draft.enrollment_id)
      .await
      .unwrap_err()
      .code,
    "vpn_cleanup_failed"
  );
  assert!(registry.get("window-one", &draft.enrollment_id).is_ok());
  runtime.fail_forget.store(false, Ordering::SeqCst);
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert!(registry.get("window-one", &draft.enrollment_id).is_err());
}

#[tokio::test]
async fn capability_and_disconnection_errors_remain_actionable() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let unsupported = Arc::new(FakeRuntime::default());
  unsupported.fail_support.store(true, Ordering::SeqCst);
  let draft = begin(&registry, &fixture, &unsupported);
  started(&registry, &draft.enrollment_id).await;
  let status = registry
    .status("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(status.error.unwrap().code, "vpn_provider_unsupported");
  assert_eq!(unsupported.start_count.load(Ordering::SeqCst), 0);
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(unsupported.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(unsupported.forget_count.load(Ordering::SeqCst), 0);
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  lock(&runtime.snapshot).connections.clear();
  let status = registry
    .status("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(status.error.unwrap().code, "vpn_enrollment_disconnected");
}

#[tokio::test]
async fn persisted_profile_protects_identity_even_when_save_reply_was_interrupted() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  let connection = lock(&runtime.started).clone().unwrap();
  Repository::new(fixture.0.clone())
    .save_enrollment(None, connection)
    .unwrap();
  registry.close_window("another-window").await;
  assert!(registry.get("window-one", &draft.enrollment_id).is_ok());
  registry.close_window("window-one").await;
  assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_aborted_save_caller_cannot_release_identity_before_its_disk_write_finishes() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  let entry = registry.get("window-one", &draft.enrollment_id).unwrap();
  let (writing, write_started) = tokio::sync::oneshot::channel();
  let (release, wait) = std::sync::mpsc::channel();
  let saving = tokio::spawn(async move {
    entry
      .save_with(None, move |directory, revision, connection| {
        writing.send(()).unwrap();
        wait.recv().unwrap();
        Repository::new(directory).save_enrollment(revision.as_deref(), connection)
      })
      .await
  });
  write_started.await.unwrap();
  saving.abort();
  assert!(saving.await.unwrap_err().is_cancelled());
  let mut cancelling = Box::pin(registry.cancel("window-one", &draft.enrollment_id));
  assert!(
    timeout(Duration::from_millis(50), &mut cancelling)
      .await
      .is_err()
  );
  assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  release.send(()).unwrap();
  cancelling.await.unwrap();
  assert!(
    Repository::new(fixture.0.clone())
      .contains(&draft.connection_id)
      .unwrap()
  );
  assert!(lock(&runtime.snapshot).connections[0].running);
  assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 0);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn disk_failure_keeps_the_authenticated_enrollment_ready_for_save_retry() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  let entry = registry.get("window-one", &draft.enrollment_id).unwrap();
  let error = entry
    .save_with(None, |_, _, _| {
      Err(CommandErrorDto::new(
        "vpn_storage_unavailable",
        "Could not save settings.",
      ))
    })
    .await
    .unwrap_err();
  assert_eq!(error.code, "vpn_storage_unavailable");
  assert!(!fixture.0.join("vpns.json").exists());
  assert!(
    registry
      .status("window-one", &draft.enrollment_id)
      .await
      .unwrap()
      .status
      .running
  );
  registry
    .save("window-one", &draft.enrollment_id, None)
    .await
    .unwrap();
  assert!(
    Repository::new(fixture.0.clone())
      .contains(&draft.connection_id)
      .unwrap()
  );
  assert_eq!(runtime.start_count.load(Ordering::SeqCst), 1);
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_keeps_shared_identity_until_passive_disappearance_then_allows_retry() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  runtime.retain_on_stop.store(true, Ordering::SeqCst);
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  assert_eq!(
    registry
      .cancel("window-one", &draft.enrollment_id)
      .await
      .unwrap_err()
      .code,
    "vpn_cleanup_pending"
  );
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  assert!(registry.get("window-one", &draft.enrollment_id).is_ok());
  assert_eq!(
    lock(&runtime.snapshot).connections[0].locally_connected,
    Some(false)
  );
  lock(&runtime.snapshot).connections.clear();
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 1);
  assert!(registry.get("window-one", &draft.enrollment_id).is_err());
}

#[tokio::test]
async fn cancellation_cannot_forget_an_identity_while_container_inventory_is_incomplete() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  lock(&runtime.snapshot).discovery_warnings = vec!["Container inventory unavailable".into()];
  assert_eq!(
    registry
      .cancel("window-one", &draft.enrollment_id)
      .await
      .unwrap_err()
      .code,
    "vpn_cleanup_pending"
  );
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  lock(&runtime.snapshot).discovery_warnings.clear();
  registry
    .cancel("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn incomplete_enrollment_inventory_never_confirms_login_or_disconnect() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  authenticate(&runtime);
  for unavailable_row in [false, true] {
    {
      let mut snapshot = lock(&runtime.snapshot);
      snapshot.connections[0].status_unavailable = unavailable_row;
      snapshot.discovery_warnings = if unavailable_row {
        vec![]
      } else {
        vec!["Container inventory unavailable".into()]
      };
    }
    assert_eq!(
      registry
        .save("window-one", &draft.enrollment_id, None)
        .await
        .unwrap_err()
        .code,
      "vpn_discovery_incomplete"
    );
    assert!(!fixture.0.join("vpns.json").exists());
  }
  {
    let mut snapshot = lock(&runtime.snapshot);
    snapshot.connections.clear();
    snapshot.discovery_warnings = vec!["Container inventory unavailable".into()];
  }
  let observation = registry
    .status("window-one", &draft.enrollment_id)
    .await
    .unwrap();
  assert!(observation.status.status_unavailable);
  assert_ne!(observation.status.state, VpnState::Stopped);
  assert!(observation.status.auth_url.is_none());
  assert!(observation.error.is_none());
}

#[tokio::test]
async fn closed_window_retries_identity_cleanup_after_passive_watchdog_grace() {
  let fixture = Fixture::new();
  let registry = Enrollments::default();
  let runtime = Arc::new(FakeRuntime::default());
  runtime.retain_on_stop.store(true, Ordering::SeqCst);
  let draft = begin(&registry, &fixture, &runtime);
  started(&registry, &draft.enrollment_id).await;
  tokio::time::pause();
  registry.close_window("window-one").await;
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
  assert!(registry.get("window-one", &draft.enrollment_id).is_ok());
  tokio::task::yield_now().await;
  lock(&runtime.snapshot).connections.clear();
  tokio::time::advance(CLOSED_WINDOW_CLEANUP_GRACE).await;
  tokio::time::resume();
  timeout(Duration::from_secs(3), async {
    while registry.get("window-one", &draft.enrollment_id).is_ok() {
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
  assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closed_window_cleanup_rechecks_adoption_and_is_bounded_while_shared_container_runs() {
  for adopt in [false, true] {
    let fixture = Fixture::new();
    let registry = Enrollments::default();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.retain_on_stop.store(true, Ordering::SeqCst);
    let draft = begin(&registry, &fixture, &runtime);
    started(&registry, &draft.enrollment_id).await;
    let entry = registry.get("window-one", &draft.enrollment_id).unwrap();
    tokio::time::pause();
    registry.close_window("window-one").await;
    tokio::task::yield_now().await;
    if adopt {
      let connection = lock(&runtime.started).clone().unwrap();
      Repository::new(fixture.0.clone())
        .save_enrollment(None, connection)
        .unwrap();
    }
    tokio::time::advance(if adopt {
      CLOSED_WINDOW_CLEANUP_GRACE
    } else {
      CLOSED_WINDOW_CLEANUP_DEADLINE
    })
    .await;
    tokio::time::resume();
    timeout(Duration::from_secs(3), async {
      while lock(&entry.state).cleanup_retry_scheduled {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    assert_eq!(runtime.forget_count.load(Ordering::SeqCst), 0);
    if adopt {
      assert!(registry.get("window-one", &draft.enrollment_id).is_err());
      assert_eq!(runtime.stop_count.load(Ordering::SeqCst), 1);
    } else {
      assert!(registry.get("window-one", &draft.enrollment_id).is_ok());
    }
  }
}
