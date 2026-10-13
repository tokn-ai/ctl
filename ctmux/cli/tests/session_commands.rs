#![cfg(unix)]

use ctmux_proto::{SplitAxis, ViewInfo, ViewLayout};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::{
  io::Read,
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
    if !self.task.is_finished() {
      let sessions = self.command(&["ls"]);
      for id in String::from_utf8_lossy(&sessions.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| line.split('\t').nth(1))
      {
        let _ = self.command(&["kill-session", "-t", id]);
      }
    }
    self.task.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

#[test]
fn resume_requires_a_terminal_before_starting_a_missing_daemon() {
  let directory =
    std::env::temp_dir().join(format!("rcli-{}", &uuid::Uuid::new_v4().to_string()[..8]));
  std::fs::create_dir(&directory).unwrap();
  let socket = directory.join("ctmux.sock");
  let output = Command::new(env!("CARGO_BIN_EXE_ctmux"))
    .args(["--resume", "--prefix", "Alt+a", "-S"])
    .arg(&socket)
    .output()
    .unwrap();
  assert!(!output.status.success());
  assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
  assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
  std::fs::remove_dir(directory).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_tui_configuration_prevents_interactive_creation_but_allows_detached() {
  let daemon = Daemon::start().await;
  let config = daemon.directory.join("invalid-ctmux.json");
  std::fs::write(&config, r#"{"selection":{"mouse_release":"typo"}}"#).unwrap();
  let pair = native_pty_system()
    .openpty(PtySize {
      rows: 24,
      cols: 80,
      pixel_width: 0,
      pixel_height: 0,
    })
    .unwrap();
  let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_ctmux"));
  command.arg("-S");
  command.arg(daemon.directory.join("ctmux.sock"));
  command.args(["new", "-s", "unwanted"]);
  command.env("CTMUX_CONFIG_FILE", &config);
  let mut reader = pair.master.try_clone_reader().unwrap();
  let output = std::thread::spawn(move || {
    let mut output = String::new();
    // PTYs may report EIO instead of EOF after their final slave closes.
    let _ = reader.read_to_string(&mut output);
    output
  });
  let mut child = pair.slave.spawn_command(command).unwrap();
  drop(pair.slave);
  let status = timeout(Duration::from_secs(5), async {
    loop {
      if let Some(status) = child.try_wait().unwrap() {
        return status;
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await;
  if status.is_err() {
    let _ = child.kill();
    let _ = child.wait();
  }
  let output = output.join().unwrap();
  let status = status.expect("invalid configuration must fail before opening the TUI");
  assert!(!status.success());
  assert!(output.contains("invalid-ctmux.json"), "{output}");
  assert!(output.contains("typo"), "{output}");
  assert_eq!(daemon.success(&["ls"]).lines().count(), 1);

  let detached = Command::new(env!("CARGO_BIN_EXE_ctmux"))
    .env("CTMUX_CONFIG_FILE", &config)
    .arg("-S")
    .arg(daemon.directory.join("ctmux.sock"))
    .args(["new", "-ds", "detached", "--", "/bin/sh"])
    .output()
    .unwrap();
  assert!(
    detached.status.success(),
    "{}",
    String::from_utf8_lossy(&detached.stderr)
  );
  assert!(String::from_utf8_lossy(&detached.stdout).contains("\tdetached\n"));
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
    &["--resume"][..],
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
async fn explicitly_empty_new_session_names_use_distinct_automatic_names() {
  let daemon = Daemon::start().await;
  let first = daemon.success(&["new", "-ds", "", "--", "/bin/sh"]);
  let second = daemon.success(&["new", "-ds", "", "--", "/bin/sh"]);
  let (first_id, first_name) = first.trim().split_once('\t').unwrap();
  let (second_id, second_name) = second.trim().split_once('\t').unwrap();
  assert_ne!(first_name, "");
  assert_ne!(second_name, "");
  assert_ne!(first_name, second_name);
  assert_ne!(first_id, second_id);
  daemon.success(&["kill", "-t", first_id]);
  daemon.success(&["kill", "-t", second_id]);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_resolves_names_and_ids_without_replacing_persistent_work() {
  let daemon = Daemon::start().await;
  daemon.success(&["new", "-ds", "work", "--", "/bin/sh"]);
  daemon.success(&["new", "-ds", "other", "--", "/bin/sh"]);
  let original = daemon.view(&["view", "work"]);
  let duplicate = daemon.command(&["rename-session", "-t", "work", "other"]);
  assert!(!duplicate.status.success());
  assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already exists"));
  daemon.success(&["kill", "other"]);
  timeout(Duration::from_secs(5), async {
    while daemon.success(&["ls"]).lines().count() != 2 {
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("terminated session releases its name before reuse");
  assert_eq!(
    daemon.success(&["rename-session", "-t", "work", "other"]),
    format!("{}\tother\n", original.session_id)
  );
  let renamed = daemon.view(&["view", "other"]);
  assert_eq!(renamed.session_id, original.session_id);
  assert_eq!(renamed.layout, original.layout);
  assert_eq!(renamed.terminals.len(), original.terminals.len());
  assert_eq!(
    renamed.terminals[0].terminal_id,
    original.terminals[0].terminal_id
  );
  assert_eq!(
    renamed.terminals[0].created_at_ms,
    original.terminals[0].created_at_ms
  );
  assert!(!daemon.command(&["view", "work"]).status.success());
  assert_eq!(
    daemon.success(&["rename", "-t", &original.session_id, "work"]),
    format!("{}\twork\n", original.session_id)
  );
  assert!(
    !daemon
      .command(&["rename", "-t", "missing", "other"])
      .status
      .success()
  );
  assert!(
    !daemon
      .command(&["rename", "-t", "work", ""])
      .status
      .success()
  );
  assert_eq!(daemon.success(&["ls"]).lines().count(), 2);
  daemon.success(&["kill", "work"]);
}
