#![cfg(unix)]

use ctmux_proto::{SplitAxis, ViewInfo, ViewLayout};
use std::{
  os::unix::fs::PermissionsExt,
  path::PathBuf,
  process::{Command, Output},
  time::Duration,
};
use tokio::time::{sleep, timeout};

struct Daemon {
  directory: PathBuf,
  task: tokio::task::JoinHandle<Result<(), ctmuxd::DaemonError>>,
}

impl Daemon {
  async fn start() -> Self {
    let directory =
      std::env::temp_dir().join(format!("rcli-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = directory.join("ctmux.sock");
    let config = ctmuxd::DaemonConfig {
      socket_path: socket_path.clone(),
      ..Default::default()
    };
    let task = tokio::spawn(ctmuxd::run(config));
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
    Command::new(env!("CARGO_BIN_EXE_ctmux"))
      .env(
        "CTMUX_ARCHIVE_DIRECTORY",
        self.directory.join("client-archives"),
      )
      .arg("-S")
      .arg(self.directory.join("ctmux.sock"))
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

  fn view(&self, args: &[&str]) -> ViewInfo {
    serde_json::from_str(&self.success(args)).unwrap()
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
  // Archive reads are local and still work after the daemon is gone.
  daemon.task.abort();
  let archives = daemon.success(&["archives"]);
  assert!(archives.contains("work"));
  assert!(archives.contains("other"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_commands_preserve_layout_selectors_argv_and_cwd() {
  let daemon = Daemon::start().await;
  daemon.success(&["new", "-ds", "work", "--", "/bin/sh"]);
  let original = daemon.view(&["view", "work"]);
  assert_eq!(original.terminals.len(), 1);
  let original_terminal = &original.terminals[0].terminal_id;

  let cwd = daemon.directory.to_str().unwrap();
  let split = daemon.view(&[
    "split",
    original_terminal,
    "--vertical",
    "--cwd",
    cwd,
    "--",
    "/bin/sh",
    "-c",
    "printf '%s\\n' \"$1\" > split-argv.txt; exec /bin/sh",
    "ctl-split",
    "argument with spaces",
  ]);
  assert_eq!(split.session_id, original.session_id);
  assert!(split.revision > original.revision);
  assert_eq!(split.terminals.len(), 2);
  assert!(matches!(
    split.layout,
    ViewLayout::Split {
      axis: SplitAxis::Vertical,
      ..
    }
  ));
  timeout(Duration::from_secs(5), async {
    while std::fs::read_to_string(daemon.directory.join("split-argv.txt"))
      .ok()
      .as_deref()
      != Some("argument with spaces\n")
    {
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("split command preserves its arguments and working directory");
  let split_terminal = split
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != *original_terminal)
    .unwrap()
    .terminal_id
    .clone();

  let promoted = daemon.view(&["promote", &split_terminal, "--name", "other"]);
  assert_eq!(promoted.session_name, "other");
  assert_ne!(promoted.session_id, original.session_id);
  assert_eq!(promoted.terminals.len(), 1);
  assert_eq!(promoted.terminals[0].terminal_id, split_terminal);
  assert_eq!(
    daemon.view(&["view", &original.session_id]).terminals.len(),
    1
  );

  let merged = daemon.view(&["merge", &promoted.session_id, "work"]);
  assert_eq!(merged.session_id, original.session_id);
  assert_eq!(merged.terminals.len(), 2);
  assert!(
    !daemon
      .command(&["view", &promoted.session_id])
      .status
      .success()
  );

  assert_eq!(daemon.success(&["kill-terminal", &split_terminal]), "");
  // Kill acknowledges the signal; the child waiter then updates the layout.
  let remaining = timeout(Duration::from_secs(5), async {
    loop {
      let view = daemon.view(&["view", "work"]);
      if view.terminals.len() == 1 {
        break view;
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("terminated terminal leaves its siblings running");
  assert_eq!(remaining.terminals.len(), 1);
  assert_eq!(remaining.terminals[0].terminal_id, *original_terminal);
  assert!(
    !daemon
      .command(&["kill-terminal", &split_terminal])
      .status
      .success()
  );
}
