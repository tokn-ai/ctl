#![cfg(unix)]

use std::{
  os::unix::fs::PermissionsExt,
  path::PathBuf,
  process::{Command, Output},
  time::Duration,
};
use tokio::time::{sleep, timeout};

struct Daemon {
  directory: PathBuf,
  task: tokio::task::JoinHandle<Result<(), rmuxd::DaemonError>>,
}

impl Daemon {
  async fn start() -> Self {
    let directory =
      std::env::temp_dir().join(format!("rcli-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = directory.join("rmux.sock");
    let config = rmuxd::DaemonConfig {
      socket_path: socket_path.clone(),
      ..Default::default()
    };
    let task = tokio::spawn(rmuxd::run(config));
    timeout(Duration::from_secs(5), async {
      while !socket_path.exists() {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    Self { directory, task }
  }

  fn command(&self, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rmux"))
      .arg("-S")
      .arg(self.directory.join("rmux.sock"))
      .args(args)
      .output()
      .unwrap()
  }

  fn success(&self, args: &[&str]) -> String {
    let output = self.command(args);
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
  }
}

impl Drop for Daemon {
  fn drop(&mut self) {
    for name in ["work", "other"] {
      let _ = self.command(&["kill-session", "-t", name]);
    }
    self.task.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tmux_create_attach_existing_and_noninteractive_safety() {
  let daemon = Daemon::start().await;
  let first = daemon.success(&["new", "-ds", "work", "--", "/bin/sh"]);
  assert!(first.contains("\twork\n"));
  assert_eq!(daemon.success(&["new-session", "-Ads", "work"]), first);
  assert!(!daemon.command(&["new", "-ds", "work"]).status.success());
  daemon.success(&["new", "-Ads", "other", "--", "/bin/sh"]);
  let listed = daemon.success(&["ls"]);
  assert_eq!(listed.lines().count(), 3);
  assert!(listed.contains("work"));
  assert!(listed.contains("other"));

  // Interactive forms fail before creating any session when output is redirected.
  for args in [
    &[][..],
    &["new", "-s", "unwanted"][..],
    &["attach", "-t", "work"][..],
  ] {
    let output = daemon.command(args);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
  }
  assert_eq!(daemon.success(&["list-sessions"]).lines().count(), 3);
  daemon.success(&["kill-session", "-t", "other"]);
  daemon.success(&["kill", "work"]);
}
