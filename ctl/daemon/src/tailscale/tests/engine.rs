use std::fs;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use rustix::process::{Pid, Signal, kill_process};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const LOGIN: &str = r#"{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/example","Self":null,"User":null,"CurrentTailnet":null}"#;

struct Engine {
  root: PathBuf,
  executable: PathBuf,
  proxy: JoinHandle<()>,
  port: u16,
}

impl Engine {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctld-tailscale-test-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let executable = root.join("engine");
    std::os::unix::fs::symlink(
      concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/tailscale/fixtures/engine.sh"
      ),
      &executable,
    )
    .unwrap();
    fs::write(root.join("status.json"), LOGIN).unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(root.join("port"), port.to_string()).unwrap();
    let proxy = tokio::spawn(async move {
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        if stream.read_exact(&mut greeting).await.is_err() {
          continue;
        }
        assert_eq!(greeting, [5, 1, 0]);
        if stream.write_all(&[5, 0]).await.is_err() {
          continue;
        }
        let mut request = [0; 10];
        if stream.read_exact(&mut request).await.is_err() {
          continue;
        }
        assert_eq!(request, [5, 3, 0, 1, 0, 0, 0, 0, 0, 0]);
        if stream
          .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 123, 45])
          .await
          .is_err()
        {
          continue;
        }
        assert_eq!(stream.read(&mut [0; 1]).await.unwrap(), 0);
      }
    });
    Self {
      root,
      executable,
      proxy,
      port,
    }
  }

  fn mark(&self, name: &str) {
    fs::write(self.root.join(name), "").unwrap();
  }

  fn calls(&self, name: &str) -> usize {
    fs::read_to_string(self.root.join(name))
      .unwrap_or_default()
      .lines()
      .count()
  }

  async fn wait_file(&self, name: &str) {
    timeout(TEST_TIMEOUT, async {
      while !self.root.join(name).exists() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
  }

  async fn wait_exit(&self) {
    timeout(TEST_TIMEOUT, async {
      while self.root.join("container").exists() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
  }

  async fn start(&self) -> ManagedVpn {
    timeout(
      TEST_TIMEOUT,
      start_config(config("first"), &self.executable),
    )
    .await
    .unwrap()
    .unwrap()
  }
}

impl Drop for Engine {
  fn drop(&mut self) {
    self.proxy.abort();
    if let Some(pid) = fs::read_to_string(self.root.join("watchdog.pid"))
      .ok()
      .and_then(|pid| pid.parse().ok())
      .and_then(Pid::from_raw)
    {
      let _ = kill_process(pid, Signal::TERM);
    }
    let _ = fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn login_updates_after_authentication_and_expiry_without_owning_container_shutdown() {
  let engine = Engine::new();
  let mut vpn = engine.start().await;
  assert_eq!(vpn.status().state, VpnState::Starting);
  assert!(vpn.status().auth_url.is_some());
  assert!(vpn.status().endpoint.is_none());
  assert_eq!(vpn.status().locally_connected, Some(true));
  fs::write(
    engine.root.join("status.json"),
    r#"{"BackendState":"Running"}"#,
  )
  .unwrap();
  timeout(TEST_TIMEOUT, async {
    while !vpn.status().running {
      vpn.status.changed().await.unwrap();
    }
  })
  .await
  .unwrap();
  assert_eq!(
    vpn.status().endpoint.as_deref(),
    Some(format!("socks5h://127.0.0.1:{}", engine.port).as_str())
  );
  fs::write(engine.root.join("status.json"), LOGIN).unwrap();
  timeout(TEST_TIMEOUT, async {
    while vpn.status().running {
      vpn.status.changed().await.unwrap();
    }
  })
  .await
  .unwrap();
  assert!(vpn.status().endpoint.is_none());
  assert!(vpn.status().auth_url.is_some());
  vpn.shutdown().await;
  assert!(!engine.root.join("remove.args").exists());
  engine.wait_exit().await;
  let args = fs::read_to_string(engine.root.join("run.args")).unwrap();
  assert!(args.contains("--detach\n"));
  assert!(!args.contains("--interactive\n"));
  assert!(args.contains("--publish\n127.0.0.1::1080/tcp\n"));
  assert!(args.contains("--volume\nctld-tailscale-"));
  assert!(args.contains("-state:/state\n"));
  assert!(!args.contains("--device\n"));
  assert!(!args.contains("--cap-add\n"));
  assert!(!args.contains("TS_AUTHKEY="));
}

#[tokio::test]
async fn two_daemons_reuse_one_container_and_disconnect_releases_only_one_interest() {
  let engine = Engine::new();
  let mut first = engine.start().await;
  let mut second = engine.start().await;
  assert_eq!(first.container.id, second.container.id);
  assert_eq!(first.container.port, second.container.port);
  assert_eq!(engine.calls("run.calls"), 1);
  first.shutdown().await;
  let prior = engine.calls("heartbeats");
  sleep(Duration::from_millis(4500)).await;
  assert!(engine.root.join("container").exists());
  assert!(engine.calls("heartbeats") > prior);
  assert!(!engine.root.join("remove.args").exists());
  second.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn lost_creator_cli_and_dropped_creator_do_not_stop_an_adopting_daemon() {
  let engine = Engine::new();
  engine.mark("creator_client_failed");
  let first = engine.start().await;
  let mut second = engine.start().await;
  drop(first);
  sleep(Duration::from_millis(4500)).await;
  assert!(engine.root.join("container").exists());
  assert_eq!(engine.calls("run.calls"), 1);
  assert!(!engine.root.join("remove.args").exists());
  second.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn concurrent_creators_adopt_the_atomic_name_winner() {
  let engine = Engine::new();
  let (first, second) = tokio::join!(engine.start(), engine.start());
  assert_eq!(first.container.id, second.container.id);
  assert_eq!(engine.calls("created.calls"), 1);
  drop(first);
  drop(second);
  engine.wait_exit().await;
}

#[tokio::test]
async fn different_settings_refuse_reuse_and_leave_the_existing_device_unchanged() {
  let engine = Engine::new();
  let mut first = engine.start().await;
  let original = fs::read_to_string(engine.root.join("container/labels")).unwrap();
  let changed = Config::from_connection(&VpnConnection {
    connection_id: "first".into(),
    name: "Changed device".into(),
    settings: VpnSettings::Tailscale {
      hostname: Some("changed-device".into()),
      accept_routes: true,
    },
  })
  .unwrap();
  let error = start_config(changed, &engine.executable)
    .await
    .err()
    .unwrap();
  assert!(error.to_string().contains("different public settings"));
  assert_eq!(
    fs::read_to_string(engine.root.join("container/labels")).unwrap(),
    original
  );
  assert_eq!(engine.calls("run.calls"), 1);
  assert!(!engine.root.join("remove.args").exists());
  first.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn cancelling_running_startup_only_releases_its_interest() {
  let engine = Engine::new();
  engine.mark("no_status");
  let (cancel, cancellation) = oneshot::channel();
  let mut settings = config("first");
  settings.cancellation = Some(cancellation);
  let mut startup = Box::pin(start_config(settings, &engine.executable));
  tokio::select! {
    result = &mut startup => panic!("startup completed: {}", result.is_ok()),
    () = engine.wait_file("heartbeats") => {},
  }
  cancel.send(()).unwrap();
  assert_eq!(
    timeout(TEST_TIMEOUT, startup)
      .await
      .unwrap()
      .err()
      .unwrap()
      .kind(),
    io::ErrorKind::Interrupted
  );
  assert!(engine.root.join("container").exists());
  assert!(!engine.root.join("remove.args").exists());
  fs::remove_file(engine.root.join("no_status")).unwrap();
  let mut adopted = engine.start().await;
  assert_eq!(engine.calls("run.calls"), 1);
  adopted.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn dropping_a_pending_start_releases_heartbeats_without_removing_running_container() {
  let engine = Engine::new();
  engine.mark("no_status");
  let mut startup = Box::pin(start_config(config("first"), &engine.executable));
  tokio::select! {
    result = &mut startup => panic!("startup completed: {}", result.is_ok()),
    () = engine.wait_file("heartbeats") => {},
  }
  drop(startup);
  assert!(!engine.root.join("remove.args").exists());
  engine.wait_exit().await;
}

#[tokio::test]
async fn cancellation_removes_only_its_unstarted_reservation_without_force() {
  let engine = Engine::new();
  engine.mark("created_only");
  let (cancel, cancellation) = oneshot::channel();
  let mut settings = config("first");
  settings.cancellation = Some(cancellation);
  let mut startup = Box::pin(start_config(settings, &engine.executable));
  tokio::select! {
    result = &mut startup => panic!("startup completed: {}", result.is_ok()),
    () = engine.wait_file("container/labels") => {},
  }
  cancel.send(()).unwrap();
  assert!(startup.await.is_err());
  engine.wait_exit().await;
  assert_eq!(
    fs::read_to_string(engine.root.join("remove.args")).unwrap(),
    format!("rm\n{}\n", "a".repeat(64))
  );
}

#[tokio::test]
async fn legacy_container_is_not_adopted_or_removed() {
  let engine = Engine::new();
  engine.mark("legacy");
  assert!(engine.start_result().await.is_err());
  assert!(engine.root.join("container").exists());
  assert!(!engine.root.join("heartbeats").exists());
  assert!(!engine.root.join("remove.args").exists());
}

#[tokio::test]
async fn an_unavailable_proxy_withdraws_an_authenticated_endpoint() {
  let engine = Engine::new();
  fs::write(
    engine.root.join("status.json"),
    r#"{"BackendState":"Running"}"#,
  )
  .unwrap();
  let mut vpn = engine.start().await;
  assert!(vpn.status().running);
  engine.proxy.abort();
  timeout(TEST_TIMEOUT, async {
    while vpn.status().running {
      vpn.status.changed().await.unwrap();
    }
  })
  .await
  .unwrap();
  assert!(vpn.status().endpoint.is_none());
  assert!(vpn.status().message.unwrap().contains("SOCKS5"));
  vpn.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn discovered_status_is_passive_and_reports_browser_sign_in() {
  let engine = Engine::new();
  let mut vpn = engine.start().await;
  vpn.shutdown().await;
  let prior = engine.calls("heartbeats");
  let status = discovered_status(
    &engine.executable,
    &vpn.container.id,
    &vpn.container.name,
    engine.port,
  )
  .await;
  assert!(status.auth_url.is_some());
  assert_eq!(engine.calls("heartbeats"), prior);
  engine.wait_exit().await;
}

#[tokio::test]
async fn exit_observation_survives_repeated_polling_and_temporary_engine_failure() {
  let engine = Engine::new();
  let mut vpn = engine.start().await;
  for _ in 0..4 {
    assert!(
      timeout(Duration::from_millis(25), vpn.exited())
        .await
        .is_err()
    );
  }
  engine.mark("engine_unavailable");
  assert!(
    timeout(Duration::from_millis(700), vpn.exited())
      .await
      .is_err()
  );
  fs::remove_file(engine.root.join("engine_unavailable")).unwrap();
  fs::remove_dir_all(engine.root.join("container")).unwrap();
  assert!(
    timeout(Duration::from_secs(2), vpn.exited())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
  let inspections = fs::read_to_string(engine.root.join("inspect.calls")).unwrap();
  assert!(
    inspections
      .lines()
      .any(|identity| identity == "a".repeat(64))
  );
  vpn.shutdown().await;
}

#[tokio::test]
async fn forgetting_identity_rejects_shared_devices_and_never_forces_state_removal() {
  let engine = Engine::new();
  let volume = format!("{}-state", config("first").container_name);
  fs::write(engine.root.join("volume"), format!("{volume}\n")).unwrap();
  let mut first = engine.start().await;
  let mut second = engine.start().await;
  first.shutdown().await;
  let result = forget_identity_with_engine("first", &engine.executable).await;
  assert!(result.unwrap_err().to_string().contains("still owned"));
  assert!(!engine.root.join("volume-remove.args").exists());
  second.shutdown().await;
  engine.wait_exit().await;
  engine.mark("volume_in_use");
  assert!(
    forget_identity_with_engine("first", &engine.executable)
      .await
      .unwrap_err()
      .to_string()
      .contains("still be in use")
  );
  assert!(engine.root.join("volume").exists());
  fs::remove_file(engine.root.join("volume_in_use")).unwrap();
  forget_identity_with_engine("first", &engine.executable)
    .await
    .unwrap();
  assert!(!engine.root.join("volume").exists());
  assert!(
    !fs::read_to_string(engine.root.join("volume-remove.args"))
      .unwrap()
      .contains("--force")
  );
}

#[tokio::test]
async fn engine_failure_is_inconclusive_and_cannot_create_or_forget_identity() {
  let engine = Engine::new();
  engine.mark("engine_unavailable");
  assert!(engine.start_result().await.is_err());
  assert!(
    forget_identity_with_engine("first", &engine.executable)
      .await
      .is_err()
  );
  assert!(!engine.root.join("run.args").exists());
  assert!(!engine.root.join("volume-remove.args").exists());
}

#[tokio::test]
async fn forgetting_a_draft_never_removes_another_profiles_identity() {
  let engine = Engine::new();
  let volume = format!("{}-state", config("saved-profile").container_name);
  fs::write(engine.root.join("volume"), format!("{volume}\n")).unwrap();
  forget_identity_with_engine("cancelled-draft", &engine.executable)
    .await
    .unwrap();
  assert!(engine.root.join("volume").exists());
  assert!(!engine.root.join("volume-remove.args").exists());
}

impl Engine {
  async fn start_result(&self) -> io::Result<ManagedVpn> {
    timeout(
      TEST_TIMEOUT,
      start_config(config("first"), &self.executable),
    )
    .await
    .unwrap()
  }
}
