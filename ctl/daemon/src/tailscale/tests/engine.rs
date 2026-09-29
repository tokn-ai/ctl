use std::fs;

use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(8);
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
        stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).await.unwrap();
      }
    });
    Self {
      root,
      executable,
      proxy,
      port,
    }
  }

  fn pid(&self) -> Option<Pid> {
    fs::read_to_string(self.root.join("run.pid"))
      .ok()
      .and_then(|pid| pid.parse().ok())
      .and_then(Pid::from_raw)
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
    let pid = self.pid().unwrap();
    timeout(TEST_TIMEOUT, async {
      while test_kill_process(pid).is_ok() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    fs::remove_file(self.root.join("run.pid")).unwrap();
  }
}

impl Drop for Engine {
  fn drop(&mut self) {
    self.proxy.abort();
    if let Some(pid) = self.pid() {
      let _ = kill_process(pid, Signal::KILL);
    }
    let _ = fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn login_is_owned_before_authentication_and_updates_after_login_and_expiry() {
  let engine = Engine::new();
  let mut vpn = timeout(
    TEST_TIMEOUT,
    start_config(config("first"), &engine.executable),
  )
  .await
  .unwrap()
  .unwrap();
  assert_eq!(vpn.status().state, VpnState::Starting);
  assert!(vpn.status().auth_url.is_some());
  assert!(vpn.status().endpoint.is_none());
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
  engine.wait_exit().await;
  assert_eq!(
    fs::read_to_string(engine.root.join("remove.args")).unwrap(),
    "rm\n--force\ntest-container-id\n"
  );
  let args = fs::read_to_string(engine.root.join("run.args")).unwrap();
  assert!(args.contains("--publish\n127.0.0.1::1080/tcp\n"));
  assert!(args.contains("--volume\nctld-tailscale-"));
  assert!(args.contains("-state:/state\n"));
  assert!(args.contains(IMAGE));
  assert!(!args.contains("--device\n"));
  assert!(!args.contains("--cap-add\n"));
  assert!(!args.contains("TS_AUTHKEY="));
}

#[tokio::test]
async fn stop_while_waiting_for_browser_keeps_persistent_state_for_the_next_start() {
  let engine = Engine::new();
  let mut vpn = start_config(config("first"), &engine.executable)
    .await
    .unwrap();
  let first_args = fs::read_to_string(engine.root.join("run.args")).unwrap();
  vpn.shutdown().await;
  engine.wait_exit().await;
  let mut vpn = start_config(config("first"), &engine.executable)
    .await
    .unwrap();
  let second_args = fs::read_to_string(engine.root.join("run.args")).unwrap();
  let volume = |args: &str| {
    args
      .lines()
      .find(|line| line.ends_with("-state:/state"))
      .unwrap()
      .to_owned()
  };
  assert_eq!(volume(&first_args), volume(&second_args));
  vpn.shutdown().await;
  engine.wait_exit().await;
  assert!(
    !fs::read_to_string(engine.root.join("remove.args"))
      .unwrap()
      .contains("--volumes")
  );
}

#[tokio::test]
async fn cancelled_start_and_dropped_pending_lease_release_heartbeats() {
  let engine = Engine::new();
  fs::write(engine.root.join("no_status"), "").unwrap();
  let mut startup = Box::pin(start_config(config("first"), &engine.executable));
  tokio::select! {
    result = &mut startup => panic!("startup returned before service readiness: {}", result.is_ok()),
    () = engine.wait_file("heartbeats") => {},
  }
  drop(startup);
  engine.wait_exit().await;
  fs::remove_file(engine.root.join("no_status")).unwrap();
  let vpn = start_config(config("first"), &engine.executable)
    .await
    .unwrap();
  let heartbeat = vpn.heartbeat.abort_handle();
  let monitor = vpn.monitor.as_ref().unwrap().abort_handle();
  drop(vpn);
  engine.wait_exit().await;
  timeout(TEST_TIMEOUT, async {
    while !heartbeat.is_finished() || !monitor.is_finished() {
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn competing_owner_cannot_be_adopted_or_removed() {
  let engine = Engine::new();
  fs::write(engine.root.join("competing"), "").unwrap();
  let result = timeout(
    TEST_TIMEOUT,
    start_config(config("first"), &engine.executable),
  )
  .await
  .unwrap();
  assert!(result.is_err());
  assert!(!engine.root.join("remove.args").exists());
}

#[tokio::test]
async fn an_unavailable_socks_listener_withdraws_an_authenticated_endpoint() {
  let engine = Engine::new();
  fs::write(
    engine.root.join("status.json"),
    r#"{"BackendState":"Running"}"#,
  )
  .unwrap();
  let mut vpn = start_config(config("first"), &engine.executable)
    .await
    .unwrap();
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
async fn startup_waits_for_actionable_login_and_cancellation_waits_for_container_cleanup() {
  for state in [
    r#"{"BackendState":"Stopped"}"#,
    r#"{"BackendState":"NeedsLogin","AuthURL":""}"#,
  ] {
    let engine = Engine::new();
    fs::write(engine.root.join("status.json"), state).unwrap();
    let mut startup = Box::pin(start_config(config("first"), &engine.executable));
    tokio::select! {
      result = &mut startup => panic!("transient status was returned: {}", result.is_ok()),
      () = engine.wait_file("heartbeats") => {},
    }
    assert!(
      timeout(Duration::from_millis(50), &mut startup)
        .await
        .is_err()
    );
    let next = if state.contains("Stopped") {
      r#"{"BackendState":"Running"}"#
    } else {
      LOGIN
    };
    fs::write(engine.root.join("status.json"), next).unwrap();
    let mut vpn = timeout(TEST_TIMEOUT, startup).await.unwrap().unwrap();
    assert!(vpn.status().running || vpn.status().auth_url.is_some());
    vpn.shutdown().await;
    engine.wait_exit().await;
  }

  let engine = Engine::new();
  fs::write(
    engine.root.join("status.json"),
    r#"{"BackendState":"Stopped"}"#,
  )
  .unwrap();
  let (cancel, cancellation) = oneshot::channel();
  fs::write(engine.root.join("separate_container"), "").unwrap();
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
  assert!(engine.root.join("remove.args").exists());
  assert!(!engine.root.join("container.running").exists());
  engine.wait_exit().await;
  fs::write(engine.root.join("status.json"), LOGIN).unwrap();
  let mut restarted = start_config(config("first"), &engine.executable)
    .await
    .unwrap();
  restarted.shutdown().await;
  engine.wait_exit().await;
}

#[tokio::test]
async fn cancellation_removes_a_container_created_after_the_attached_client_exits() {
  let engine = Engine::new();
  fs::write(engine.root.join("late_create"), "").unwrap();
  let (cancel, cancellation) = oneshot::channel();
  let mut settings = config("first");
  settings.cancellation = Some(cancellation);
  let mut startup = Box::pin(start_config(settings, &engine.executable));
  tokio::select! {
    result = &mut startup => panic!("startup completed: {}", result.is_ok()),
    () = async { engine.wait_file("first_inspect").await; engine.wait_file("heartbeats").await; } => {},
  }
  // The first lookup found nothing. Let the engine publish its already accepted
  // create request during post-client cleanup, without starting a watchdog.
  fs::write(engine.root.join("create_after_client_exit"), "").unwrap();
  cancel.send(()).unwrap();
  assert!(timeout(TEST_TIMEOUT, startup).await.unwrap().is_err());
  assert!(engine.root.join("remove.args").exists());
  assert!(!engine.root.join("container.running").exists());
  engine.wait_exit().await;
}

#[tokio::test]
async fn forgetting_identity_requires_no_container_and_preserves_in_use_volumes() {
  let engine = Engine::new();
  let volume = format!("{}-state", config("cancelled-enrollment").container_name);
  fs::write(engine.root.join("volume"), format!("{volume}\n")).unwrap();
  fs::write(engine.root.join("container.running"), "").unwrap();
  let result = forget_identity_with_engine("cancelled-enrollment", &engine.executable).await;
  assert!(result.unwrap_err().to_string().contains("still owned"));
  assert!(!engine.root.join("volume-remove.args").exists());
  fs::remove_file(engine.root.join("container.running")).unwrap();
  fs::write(engine.root.join("volume_in_use"), "").unwrap();
  let result = forget_identity_with_engine("cancelled-enrollment", &engine.executable).await;
  assert!(result.unwrap_err().to_string().contains("still be in use"));
  assert!(engine.root.join("volume").exists());
  let arguments = fs::read_to_string(engine.root.join("volume-remove.args")).unwrap();
  assert_eq!(arguments, format!("volume\nrm\n{volume}\n"));
  assert!(!arguments.contains("--force"));
  fs::remove_file(engine.root.join("volume_in_use")).unwrap();
  forget_identity_with_engine("cancelled-enrollment", &engine.executable)
    .await
    .unwrap();
  assert!(!engine.root.join("volume").exists());
  forget_identity_with_engine("cancelled-enrollment", &engine.executable)
    .await
    .unwrap();
}

#[tokio::test]
async fn forgetting_identity_fails_closed_on_unknown_engine_state() {
  let engine = Engine::new();
  fs::write(engine.root.join("engine_unavailable"), "").unwrap();
  assert!(
    forget_identity_with_engine("cancelled-enrollment", &engine.executable)
      .await
      .is_err()
  );
  assert!(!engine.root.join("volume-remove.args").exists());
}

#[tokio::test]
async fn forgetting_identity_never_removes_another_profiles_state() {
  let engine = Engine::new();
  let volume = format!("{}-state", config("saved-profile").container_name);
  fs::write(engine.root.join("volume"), format!("{volume}\n")).unwrap();
  forget_identity_with_engine("cancelled-enrollment", &engine.executable)
    .await
    .unwrap();
  assert!(engine.root.join("volume").exists());
  assert!(!engine.root.join("volume-remove.args").exists());
}
