#![cfg(unix)]

use ctld_ipc::lifecycle::{Client, DaemonStatus, Request, Response};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::time::{Duration, Instant, sleep, timeout};

struct IsolatedOwner(PathBuf);

impl IsolatedOwner {
  fn socket(&self) -> PathBuf {
    self.0.join("ctld.sock")
  }
}

impl Drop for IsolatedOwner {
  fn drop(&mut self) {
    let socket = self.socket();
    // Cleanup addresses only this test's random, private endpoint. The owner
    // itself performs shutdown; no default socket or process-name kill is used.
    let _ = std::thread::spawn(move || {
      let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
      runtime.block_on(async {
        let _ = timeout(Duration::from_secs(5), stop_owner(&socket)).await;
      });
    })
    .join();
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

async fn stop_owner(socket: &Path) {
  let Ok(mut stream) = ctld_ipc::connect_existing_at(socket).await else {
    return;
  };
  ctld_ipc::write_frame(
    &mut stream,
    &Request::CtldInspect {
      protocol_version: ctld_ipc::lifecycle::PROTOCOL_VERSION,
    },
  )
  .await
  .unwrap();
  let Some(Response::CtldInfo { info }) = ctld_ipc::read_frame(&mut stream).await.unwrap() else {
    return;
  };
  ctld_ipc::write_frame(
    &mut stream,
    &Request::CtldRestart {
      expected_instance_id: info.instance_id,
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, Response>(&mut stream)
      .await
      .unwrap(),
    Some(Response::CtldRestartAccepted { .. })
  ));
  assert!(
    ctld_ipc::read_frame::<_, Response>(&mut stream)
      .await
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn restart_replaces_only_an_isolated_owner_and_verifies_the_new_build() {
  let directory =
    std::env::temp_dir().join(format!("cl-{}", &uuid::Uuid::new_v4().to_string()[..8]));
  std::fs::create_dir(&directory).unwrap();
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  let owner = IsolatedOwner(directory);
  let client =
    Client::new(owner.socket()).with_daemon_executable(env!("CARGO_BIN_EXE_ctld").into());
  assert_eq!(client.probe().await.unwrap(), DaemonStatus::Absent);
  let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--socket")
    .arg(owner.socket())
    .env_remove("CTLD_ASKPASS")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let deadline = Instant::now() + Duration::from_secs(5);
  let before = loop {
    if let DaemonStatus::Running { info } = client.probe().await.unwrap() {
      break info;
    }
    assert!(Instant::now() < deadline, "isolated owner did not start");
    sleep(Duration::from_millis(20)).await;
  };
  let prepared = client.preflight_restart().await.unwrap();
  assert_eq!(prepared.before.as_ref(), Some(&before));
  let outcome = prepared.restart().await.unwrap();
  assert_ne!(outcome.after.instance_id, before.instance_id);
  assert_eq!(outcome.after.binary, client.available().await.unwrap().info);
  assert!(
    timeout(Duration::from_secs(3), child.wait())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
  stop_owner(&owner.socket()).await;
  assert_eq!(client.probe().await.unwrap(), DaemonStatus::Absent);
}

#[tokio::test]
async fn vpn_status_observes_each_inventory_once_per_request() {
  let directory =
    PathBuf::from("/tmp").join(format!("cl-vpn-{}", &uuid::Uuid::new_v4().to_string()[..8]));
  std::fs::create_dir(&directory).unwrap();
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  let owner = IsolatedOwner(directory);
  let engine = owner.0.join("docker");
  std::fs::write(
    &engine,
    "#!/bin/sh\n[ \"$1\" = ps ] || exit 1\nprintf '%s\\n' \"$*\" >> \"$0.calls\"\n",
  )
  .unwrap();
  std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
  let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--socket")
    .arg(owner.socket())
    .env("PATH", &owner.0)
    .env_remove("CTLD_ASKPASS")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let lifecycle = Client::new(owner.socket());
  timeout(Duration::from_secs(10), async {
    loop {
      if matches!(
        lifecycle.probe().await.unwrap(),
        DaemonStatus::Running { .. }
      ) {
        break;
      }
      sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .unwrap();
  // The readiness query observes lifecycle metadata independently of VPN status.
  let prior = std::fs::read_to_string(owner.0.join("docker.calls"))
    .unwrap_or_default()
    .lines()
    .count();
  let snapshot = ctld_ipc::vpn::Client::new(owner.socket())
    .list()
    .await
    .unwrap();
  assert!(snapshot.connections.is_empty());
  assert!(snapshot.discovery_warnings.is_empty());
  let calls = std::fs::read_to_string(owner.0.join("docker.calls")).unwrap();
  let calls: Vec<_> = calls.lines().skip(prior).collect();
  assert_eq!(calls.len(), 2, "each inventory is scanned once");
  assert_eq!(
    calls
      .iter()
      .filter(|call| call.contains("label=io.ctl.vpn.protocol=1"))
      .count(),
    1,
  );
  assert_eq!(
    calls
      .iter()
      .filter(|call| call.contains("label=io.ctl.service"))
      .count(),
    1,
  );
  stop_owner(&owner.socket()).await;
  assert!(
    timeout(Duration::from_secs(5), child.wait())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
}
