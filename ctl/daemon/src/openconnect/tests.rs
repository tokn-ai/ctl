use super::*;

#[test]
fn accepts_only_a_single_nonzero_loopback_port() {
  assert_eq!(
    parse_published_port(br#"{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49152"}]}"#).unwrap(),
    49152
  );
  for invalid in [
    "{}",
    r#"{"1080/tcp":null}"#,
    r#"{"1080/tcp":[]}"#,
    r#"{"1080/tcp":[{"HostIp":"0.0.0.0","HostPort":"49152"}]}"#,
    r#"{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"0"}]}"#,
    r#"{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"65536"}]}"#,
    r#"{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49152"},{"HostIp":"::1","HostPort":"49152"}]}"#,
  ] {
    assert!(parse_published_port(invalid.as_bytes()).is_err());
  }
}

#[cfg(unix)]
mod engine {
  use std::fs;
  use std::os::unix::fs::PermissionsExt as _;

  use rustix::process::{Pid, Signal, kill_process, test_kill_process};

  use super::*;

  const TEST_TIMEOUT: Duration = Duration::from_secs(5);
  const FAKE_ENGINE: &str = r#"#!/bin/sh
set -eu
root=${0%/*}
case "$1" in
  run)
    printf '%s\n' "$@" > "$root/run.args"
    printf '%s\n' "$$" > "$root/run.pid"
    while IFS= read -r heartbeat; do
      printf '%s\n' "$heartbeat" >> "$root/heartbeats"
    done
    ;;
  inspect)
    touch "$root/inspected"
    [ -f "$root/ready" ] || exit 1
    printf '%s\n' '{"1080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49152"}]}'
    ;;
  exec)
    touch "$root/healthchecked"
    [ -f "$root/ready" ]
    ;;
  rm)
    printf '%s\n' "$@" > "$root/remove.args"
    ;;
  *) exit 2 ;;
esac
"#;

  struct FakeEngine {
    root: PathBuf,
    executable: PathBuf,
    config: PathBuf,
  }

  impl FakeEngine {
    fn new() -> Self {
      let root = std::env::temp_dir().join(format!("ctld-vpn-test-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&root).unwrap();
      let executable = root.join("engine");
      fs::write(&executable, FAKE_ENGINE).unwrap();
      fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
      let config = root.join("vpn.env");
      fs::write(&config, "VPN_PASSWORD=private-test-password\n").unwrap();
      fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
      Self {
        root,
        executable,
        config,
      }
    }

    fn options(&self) -> Options {
      Options {
        env_file: self.config.clone(),
      }
    }

    fn pid(&self) -> Option<Pid> {
      fs::read_to_string(self.root.join("run.pid"))
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
        .and_then(Pid::from_raw)
    }

    async fn wait_for_file(&self, name: &str) {
      timeout(TEST_TIMEOUT, async {
        while !self.root.join(name).exists() {
          sleep(Duration::from_millis(10)).await;
        }
      })
      .await
      .unwrap_or_else(|_| panic!("fake engine did not write {name}"));
    }

    async fn wait_for_exit(&self) {
      let pid = self.pid().expect("fake engine recorded its PID");
      timeout(TEST_TIMEOUT, async {
        while test_kill_process(pid).is_ok() {
          sleep(Duration::from_millis(10)).await;
        }
      })
      .await
      .expect("attached engine process was not stopped and reaped");
      fs::remove_file(self.root.join("run.pid")).unwrap();
    }
  }

  impl Drop for FakeEngine {
    fn drop(&mut self) {
      if let Some(pid) = self.pid() {
        let _ = kill_process(pid, Signal::KILL);
      }
      let _ = fs::remove_dir_all(&self.root);
    }
  }

  #[tokio::test]
  async fn readiness_discovers_engine_assigned_port_and_shutdown_removes_container() {
    let engine = FakeEngine::new();
    let mut startup = Box::pin(start_with_engine(engine.options(), &engine.executable));
    tokio::select! {
      result = &mut startup => panic!("startup completed before VPN readiness: {}", result.is_ok()),
      () = engine.wait_for_file("heartbeats") => {},
    }
    // Successful engine attachment alone must not publish an endpoint.
    assert!(
      timeout(Duration::from_millis(50), &mut startup)
        .await
        .is_err()
    );
    fs::write(engine.root.join("ready"), "").unwrap();
    let mut vpn = timeout(TEST_TIMEOUT, startup).await.unwrap().unwrap();

    let status = vpn.status();
    assert!(status.running);
    assert_eq!(
      status.endpoint.as_deref(),
      Some("socks5h://127.0.0.1:49152")
    );
    assert!(engine.root.join("healthchecked").exists());
    let arguments = fs::read_to_string(engine.root.join("run.args")).unwrap();
    assert!(arguments.contains("--publish\n127.0.0.1::1080/tcp\n"));
    assert!(arguments.contains("--interactive\n"));
    assert!(arguments.contains("--rm\n"));
    assert!(!arguments.contains("private-test-password"));
    let heartbeats = fs::read_to_string(engine.root.join("heartbeats")).unwrap();
    assert!(!heartbeats.is_empty());
    assert!(heartbeats.lines().all(|heartbeat| heartbeat == "ping"));

    let heartbeat = vpn.heartbeat.abort_handle();
    vpn.shutdown().await;
    engine.wait_for_exit().await;
    assert!(heartbeat.is_finished());
    assert!(!vpn.status().running);
    assert!(vpn.status().endpoint.is_none());
    let removed = fs::read_to_string(engine.root.join("remove.args")).unwrap();
    assert_eq!(
      removed,
      format!("rm\n--force\n{}\n", status.container_name.unwrap())
    );
  }

  #[tokio::test]
  async fn cancelling_startup_stops_the_attached_engine_before_readiness() {
    let engine = FakeEngine::new();
    let mut startup = Box::pin(start_with_engine(engine.options(), &engine.executable));
    tokio::select! {
      result = &mut startup => panic!("startup completed before VPN readiness: {}", result.is_ok()),
      () = engine.wait_for_file("heartbeats") => {},
    }
    drop(startup);
    engine.wait_for_exit().await;
    assert!(!engine.root.join("healthchecked").exists());
  }

  #[tokio::test]
  async fn dropping_ready_lease_aborts_heartbeats_and_stops_attached_engine() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let vpn = timeout(
      TEST_TIMEOUT,
      start_with_engine(engine.options(), &engine.executable),
    )
    .await
    .unwrap()
    .unwrap();
    let heartbeat = vpn.heartbeat.abort_handle();
    drop(vpn);
    engine.wait_for_exit().await;
    assert!(heartbeat.is_finished());
  }

  #[tokio::test]
  async fn readable_by_group_config_is_rejected_before_engine_starts() {
    let engine = FakeEngine::new();
    fs::set_permissions(&engine.config, fs::Permissions::from_mode(0o640)).unwrap();
    let result = start_with_engine(engine.options(), &engine.executable).await;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::PermissionDenied));
    assert!(!engine.root.join("run.args").exists());
  }
}
