//! Run the real container entrypoint with fixture services and a failed watchdog.

use std::fs;
use std::os::unix::net::UnixListener;

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
  root: std::path::PathBuf,
}

impl Fixture {
  fn new() -> Self {
    // Keep socket paths under the macOS Unix-domain path limit.
    let root =
      std::path::PathBuf::from("/tmp").join(format!("ctld-ts-watchdog-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    fs::create_dir(root.join("run")).unwrap();
    let daemon = format!(
      "#!/bin/sh\ntrap 'printf finished > \"{}/daemon.finished\"; exit 0' TERM INT\nprintf '%s' \"$$\" > \"{}/daemon.pid\"\nprintf started > \"{}/daemon.started\"\nwhile :; do sleep 0.1; done\n",
      root.display(),
      root.display(),
      root.display(),
    );
    ctl_core::test_fixtures::shell_command(root.join("bin/tailscaled"), daemon).unwrap();
    ctl_core::test_fixtures::shell_command(
      root.join("bin/tailscale"),
      "#!/bin/sh\nprintf '%s\\n' '{\"BackendState\":\"Running\"}'\n",
    )
    .unwrap();
    fs::write(root.join("watchdog.sh"), vpn_container::WATCHDOG_SCRIPT).unwrap();
    fs::write(root.join("clock"), "100.00 0.00\n").unwrap();
    let entrypoint = ENTRYPOINT
      .replace(
        "/run/ctl/watchdog.sh",
        &root.join("watchdog.sh").to_string_lossy(),
      )
      .replace("/run/tailscale", &root.join("run").to_string_lossy())
      .replace("/state", &root.join("state").to_string_lossy());
    fs::write(root.join("entrypoint.sh"), entrypoint).unwrap();
    Self { root }
  }

  async fn wait_file(&self, name: &str) {
    timeout(TEST_TIMEOUT, async {
      while !self.root.join(name).exists() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap_or_else(|_| panic!("fixture did not publish {name}"));
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    if let Some(pid) = fs::read_to_string(self.root.join("daemon.pid"))
      .ok()
      .and_then(|pid| pid.parse().ok())
      .and_then(rustix::process::Pid::from_raw)
    {
      let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let _ = fs::remove_dir_all(&self.root);
  }
}

#[tokio::test]
async fn a_failed_watchdog_stops_tailscale_during_boot_and_after_socket_publication() {
  for published in [false, true] {
    let fixture = Fixture::new();
    let _socket =
      published.then(|| UnixListener::bind(fixture.root.join("run/tailscaled.sock")).unwrap());
    let executable_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![fixture.root.join("bin")];
    paths.extend(std::env::split_paths(&executable_path));
    let mut child = tokio::process::Command::new("/bin/sh")
      .arg(fixture.root.join("entrypoint.sh"))
      .env("PATH", std::env::join_paths(paths).unwrap())
      .env("CTLD_ACCEPT_ROUTES", "false")
      .env("CTLD_HOSTNAME", "test-device")
      .env("CTLD_HEARTBEAT_DIR", fixture.root.join("heartbeat"))
      .env("CTLD_HEARTBEAT_CLOCK_FILE", fixture.root.join("clock"))
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    // The real watchdog begins with a valid clock. Only fail it after the fake
    // daemon has installed its TERM handler, including during socket startup.
    fixture.wait_file("daemon.started").await;
    fs::remove_file(fixture.root.join("clock")).unwrap();
    let status = timeout(TEST_TIMEOUT, child.wait()).await.unwrap().unwrap();
    assert!(
      !status.success(),
      "entrypoint ignored watchdog failure: socket published={published}"
    );
    // Shutdown is bounded; allow the signaled child to finish scheduling its
    // graceful exit after the parent has returned rather than assuming ordering.
    fixture.wait_file("daemon.finished").await;
  }
}
