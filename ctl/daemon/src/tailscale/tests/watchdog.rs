//! Run the real container entrypoint with fixture services and a failed watchdog.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;

use super::*;

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
    fs::write(root.join("bin/tailscaled"), daemon).unwrap();
    fs::write(
      root.join("bin/tailscale"),
      "#!/bin/sh\nprintf '%s\\n' '{\"BackendState\":\"Running\"}'\n",
    )
    .unwrap();
    for command in ["tailscaled", "tailscale"] {
      fs::set_permissions(
        root.join("bin").join(command),
        fs::Permissions::from_mode(0o700),
      )
      .unwrap();
    }
    fs::write(root.join("watchdog.sh"), vpn_container::WATCHDOG_SCRIPT).unwrap();
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
      // The real watchdog exits when it cannot read its monotonic clock.
      .env(
        "CTLD_HEARTBEAT_CLOCK_FILE",
        fixture.root.join("missing-clock"),
      )
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let status = timeout(Duration::from_secs(5), child.wait())
      .await
      .unwrap()
      .unwrap();
    assert!(
      !status.success(),
      "entrypoint ignored watchdog failure: socket published={published}"
    );
    if fixture.root.join("daemon.started").exists() {
      assert!(
        fixture.root.join("daemon.finished").exists(),
        "tailscaled outlived the failed watchdog"
      );
    }
  }
}
