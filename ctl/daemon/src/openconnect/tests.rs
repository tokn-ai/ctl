use ctld_ipc::{VpnConnection, VpnSettings};
use zeroize::Zeroizing;

use super::*;

fn saved_connection() -> VpnConnection {
  VpnConnection {
    connection_id: "test".into(),
    name: "Test VPN".into(),
    settings: VpnSettings::Openconnect {
      url: "vpn.example.test".into(),
      username: "test-user".into(),
      password: Zeroizing::new("literal $value='quoted'\\tail#=end".into()),
      auth_method: Some("default_method".into()),
      target_ip: None,
    },
  }
}

#[test]
fn generated_config_preserves_literal_values_without_shell_quoting() {
  let connection = saved_connection();
  let env = Config::from_connection(&connection).unwrap().content;
  let VpnSettings::Openconnect { password, .. } = &connection.settings else {
    unreachable!()
  };
  assert!(
    env
      .lines()
      .any(|line| line == format!("VPN_PASSWORD={}", password.as_str()))
  );
  assert!(env.ends_with("TARGET_IP=\n"));
  assert!(!env.contains("Test VPN"));
  let mut invalid = saved_connection();
  let VpnSettings::Openconnect { password, .. } = &mut invalid.settings else {
    unreachable!()
  };
  password.push('\n');
  assert!(Config::from_connection(&invalid).is_err());
}

#[test]
#[cfg(unix)]
fn engine_search_supports_gui_paths_and_never_searches_relative_directories() {
  let paths = engine_candidates(
    Some(OsStr::new(".:relative:/custom/bin:/custom/bin")),
    Some(Path::new("/home/test")),
  );
  assert_eq!(paths[0], Path::new("/custom/bin/docker"));
  assert!(paths.iter().all(|path| path.is_absolute()));
  assert!(paths.contains(&PathBuf::from("/home/test/.local/bin/docker")));
  assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin/podman")));
  assert_eq!(
    paths
      .iter()
      .filter(|path| **path == Path::new("/custom/bin/docker"))
      .count(),
    1
  );
}

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
  use super::*;
  use rustix::process::{Pid, Signal, kill_process};
  use std::fs;
  use std::os::unix::fs::PermissionsExt as _;

  const TEST_TIMEOUT: Duration = Duration::from_secs(5);
  const FAKE_ENGINE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/openconnect/fixtures/engine.sh"
  );

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
      std::os::unix::fs::symlink(FAKE_ENGINE_PATH, &executable).unwrap();
      let config = root.join("vpn.env");
      fs::write(
        &config,
        "VPN_URL=vpn.invalid\nVPN_USERNAME=test-user\nVPN_PASSWORD=private-test-password\n",
      )
      .unwrap();
      fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
      Self {
        root,
        executable,
        config,
      }
    }
    async fn wait_file(&self, name: &str) {
      timeout(TEST_TIMEOUT, async {
        while !self.root.join(name).exists() {
          sleep(Duration::from_millis(10)).await;
        }
      })
      .await
      .unwrap_or_else(|_| panic!("fake engine did not write {name}"));
    }
    async fn start(&self) -> io::Result<ManagedVpn> {
      let (_, config) = read_file(self.config.clone()).await?;
      start_config(config, &self.executable).await
    }
  }
  impl Drop for FakeEngine {
    fn drop(&mut self) {
      if let Some(pid) = fs::read_to_string(self.root.join("run.pid"))
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
        .and_then(Pid::from_raw)
      {
        let _ = kill_process(pid, Signal::KILL);
      }
      let _ = fs::remove_dir_all(&self.root);
    }
  }

  #[tokio::test]
  async fn simultaneous_starts_adopt_the_same_atomic_container_reservation() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    fs::write(engine.root.join("delayed_publication"), "").unwrap();
    let (first, second) = timeout(TEST_TIMEOUT, async {
      tokio::join!(engine.start(), engine.start())
    })
    .await
    .unwrap();
    let mut first = first.unwrap();
    let mut second = second.unwrap();
    assert_eq!(first.status().container_id, second.status().container_id);
    assert!(engine.root.join("name_conflict").exists());
    assert!(
      fs::read_to_string(engine.root.join("unpublished.inspect"))
        .unwrap()
        .lines()
        .count()
        >= 3
    );
    assert_eq!(
      fs::read_to_string(engine.root.join("run.count"))
        .unwrap()
        .trim(),
      "1"
    );
    first.shutdown().await;
    second.shutdown().await;
    assert!(!engine.root.join("remove.args").exists());
  }

  #[tokio::test]
  async fn cancellation_awaits_cleanup_of_only_its_unstarted_reservation() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("created"), "").unwrap();
    let (_, mut config) = read_file(engine.config.clone()).await.unwrap();
    let (cancel, cancellation) = tokio::sync::oneshot::channel();
    config.cancellation = Some(cancellation);
    let executable = engine.executable.clone();
    let startup = tokio::spawn(async move { start_config(config, &executable).await });
    engine.wait_file("container.json").await;
    cancel.send(()).unwrap();
    let Err(error) = timeout(TEST_TIMEOUT, startup).await.unwrap().unwrap() else {
      panic!("startup must cancel");
    };
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let removal = fs::read_to_string(engine.root.join("remove.args")).unwrap();
    assert_eq!(removal, format!("rm\n{}", "a".repeat(64)));
    assert!(!engine.root.join("container.json").exists());
  }

  #[tokio::test]
  async fn two_daemons_reuse_one_container_and_release_only_their_own_heartbeats() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let mut first = timeout(TEST_TIMEOUT, engine.start())
      .await
      .unwrap()
      .unwrap();
    let mut second = timeout(TEST_TIMEOUT, engine.start())
      .await
      .unwrap()
      .unwrap();
    assert_eq!(first.status().container_id, second.status().container_id);
    assert_eq!(
      first.status().endpoint.as_deref(),
      Some("socks5h://127.0.0.1:49152")
    );
    assert!(first.status().shared_container);
    assert_eq!(
      fs::read_to_string(engine.root.join("run.count"))
        .unwrap()
        .trim(),
      "1"
    );
    first.shutdown().await;
    assert!(engine.root.join("container.json").exists());
    assert!(!engine.root.join("remove.args").exists());
    let before = fs::read_to_string(engine.root.join("heartbeats"))
      .unwrap()
      .lines()
      .count();
    timeout(TEST_TIMEOUT, async {
      loop {
        let after = fs::read_to_string(engine.root.join("heartbeats"))
          .unwrap()
          .lines()
          .count();
        if after > before {
          break;
        }
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    second.shutdown().await;
    assert!(!engine.root.join("remove.args").exists());
  }

  #[tokio::test]
  async fn container_observation_survives_the_creator_cli_exiting() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let mut vpn = timeout(TEST_TIMEOUT, engine.start())
      .await
      .unwrap()
      .unwrap();
    if let Some(child) = &mut vpn.child {
      child.kill().await.unwrap();
    }
    assert!(
      timeout(Duration::from_millis(50), vpn.exited())
        .await
        .is_err()
    );
    fs::write(engine.root.join("exited"), "").unwrap();
    assert!(
      timeout(TEST_TIMEOUT, vpn.exited())
        .await
        .unwrap()
        .unwrap()
        .success()
    );
    vpn.shutdown().await;
  }

  #[tokio::test]
  async fn credentials_travel_once_through_stdin_and_never_enter_container_labels() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let mut vpn = timeout(TEST_TIMEOUT, engine.start())
      .await
      .unwrap()
      .unwrap();
    engine.wait_file("input").await;
    let input = fs::read_to_string(engine.root.join("input")).unwrap();
    let decoded = BASE64.decode(input.trim()).unwrap();
    assert!(
      String::from_utf8(decoded)
        .unwrap()
        .contains("VPN_PASSWORD=private-test-password")
    );
    let args = fs::read_to_string(engine.root.join("run.args")).unwrap();
    assert!(args.contains("--sig-proxy=false"));
    assert!(args.contains("--tmpfs"));
    assert!(!args.contains("private-test-password"));
    assert!(
      !fs::read_to_string(engine.root.join("container.json"))
        .unwrap()
        .contains("private-test-password")
    );
    vpn.shutdown().await;
  }

  #[tokio::test]
  async fn status_discovery_does_not_renew_heartbeats() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let mut vpn = timeout(TEST_TIMEOUT, engine.start())
      .await
      .unwrap()
      .unwrap();
    vpn.shutdown().await;
    sleep(Duration::from_millis(100)).await;
    let before = fs::read_to_string(engine.root.join("heartbeats")).unwrap();
    let containers = vpn_container::list(&engine.executable).await.unwrap();
    assert_eq!(containers.len(), 1);
    assert_eq!(containers[0].basic_status().locally_connected, Some(false));
    assert_eq!(
      fs::read_to_string(engine.root.join("heartbeats")).unwrap(),
      before
    );
  }

  #[tokio::test]
  async fn legacy_discovery_checks_health_by_immutable_id_without_acquiring_interest() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let id = "a".repeat(64);
    let status = discovered_status_at(
      &engine.executable,
      &id,
      "ctld-vpn-legacy",
      Some(49152),
      true,
    )
    .await;
    assert_eq!(status.state, VpnState::Connected);
    assert!(status.running);
    assert_eq!(
      status.endpoint.as_deref(),
      Some("socks5h://127.0.0.1:49152")
    );
    assert_eq!(status.container_id.as_deref(), Some(id.as_str()));
    assert_eq!(status.container_name.as_deref(), Some("ctld-vpn-legacy"));
    assert_eq!(status.locally_connected, Some(false));
    assert!(!status.shared_container);
    assert!(status.connection_id.is_none());
    assert!(status.vpn_url.is_none());
    assert!(status.username.is_none());
    assert_eq!(
      fs::read_to_string(engine.root.join("exec.calls")).unwrap(),
      format!("exec\n{id}\n/usr/local/bin/vpn-healthcheck\n")
    );
    for forbidden in ["heartbeats", "input", "run.args", "remove.args"] {
      assert!(!engine.root.join(forbidden).exists());
    }
  }

  #[tokio::test]
  async fn a_running_legacy_container_without_healthy_vpn_never_publishes_a_proxy() {
    let engine = FakeEngine::new();
    let status = discovered_status_at(
      &engine.executable,
      &"a".repeat(64),
      "ctld-vpn-legacy",
      Some(49152),
      true,
    )
    .await;
    assert_eq!(status.state, VpnState::Starting);
    assert!(!status.running);
    assert!(status.endpoint.is_none());
    assert!(engine.root.join("exec.calls").exists());
    assert!(!engine.root.join("heartbeats").exists());
  }

  #[tokio::test]
  async fn legacy_discovery_never_executes_against_names_stopped_containers_or_missing_ports() {
    let engine = FakeEngine::new();
    fs::write(engine.root.join("ready"), "").unwrap();
    let id = "a".repeat(64);
    for (identity, port, running) in [
      ("ctld-vpn-legacy", Some(49152), true),
      (id.as_str(), Some(49152), false),
      (id.as_str(), None, true),
      (id.as_str(), Some(0), true),
    ] {
      let status = discovered_status_at(
        &engine.executable,
        identity,
        "ctld-vpn-legacy",
        port,
        running,
      )
      .await;
      assert!(!status.running);
      assert!(status.endpoint.is_none());
      assert_eq!(
        status.state,
        if running {
          VpnState::Starting
        } else {
          VpnState::Stopped
        }
      );
    }
    assert!(!engine.root.join("exec.calls").exists());
    assert!(!engine.root.join("heartbeats").exists());
  }

  #[tokio::test]
  async fn startup_failure_reports_a_safe_cause_without_stopping_other_containers() {
    let engine = FakeEngine::new();
    fs::write(
      engine.root.join("failure"),
      "getaddrinfo failed: vpn.invalid private-test-password",
    )
    .unwrap();
    // Failure first observes a possible concurrent owner, then awaits bounded
    // cleanup of an owned Created reservation before returning diagnostics.
    let Err(error) = timeout(TEST_TIMEOUT + CREATOR_ADOPTION_TIMEOUT, engine.start())
      .await
      .unwrap()
    else {
      panic!("expected startup failure");
    };
    assert!(error.to_string().contains("could not be resolved"));
    assert!(!error.to_string().contains("private-test-password"));
    assert!(!engine.root.join("remove.args").exists());
  }

  #[tokio::test]
  async fn readable_by_group_config_is_rejected_before_engine_starts() {
    let engine = FakeEngine::new();
    fs::set_permissions(&engine.config, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(
      matches!(engine.start().await, Err(error) if error.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(!engine.root.join("run.args").exists());
  }
}
