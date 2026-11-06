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

#[test]
fn component_info_reports_protocols_without_opening_a_socket() {
  let temp = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
  let result = Command::new(env!("CARGO_BIN_EXE_ctmux"))
    .args(["--component-info", "-S"])
    .arg(&temp)
    .output()
    .unwrap();
  assert!(result.status.success());
  let info: ctl_core::component::ComponentInfo = serde_json::from_slice(&result.stdout).unwrap();
  assert_eq!(
    info.protocols,
    vec![
      ctmux_proto::protocol_info(),
      ctmux_ipc::local_control_protocol_info()
    ]
  );
  assert!(!temp.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clients_group_live_panes_and_keep_reconnect_grace_separate_from_use() {
  use ctmux_client::{ClientIdentity, begin_attach, resume_attach};
  use ctmux_proto::{ClientMessage, write_frame};
  let daemon = Daemon::start().await;
  daemon.success(&["new", "-ds", "work", "--", "/bin/sh"]);
  daemon.success(&["new", "-ds", "spare", "--", "/bin/sh"]);
  let socket = daemon.directory.join("ctmux.sock");
  let root = daemon.view(&["view", "work"]).terminals[0]
    .terminal_id
    .clone();
  let split = daemon.view(&["split", &root, "--", "/bin/sh"]);
  let child = split
    .terminals
    .iter()
    .find(|t| t.terminal_id != root)
    .unwrap()
    .terminal_id
    .clone();
  let identity = ClientIdentity {
    name: "fixture-viewer".into(),
    version: "test".into(),
  };
  let stream = ctmux_ipc::connect_existing_daemon(&socket).await.unwrap();
  let (secondary, attached) = begin_attach(stream, &identity, viewer_request(&child))
    .await
    .unwrap();
  assert_eq!(
    daemon.usage("work").clients.unwrap()[0].connected_attachments,
    1
  );
  assert_eq!(
    daemon
      .unused()
      .into_iter()
      .map(|row| row.session.name)
      .collect::<Vec<_>>(),
    vec!["spare"]
  );
  let stream = ctmux_ipc::connect_existing_daemon(&socket).await.unwrap();
  let (mut primary, _) = begin_attach(stream, &identity, viewer_request(&root))
    .await
    .unwrap();
  let grouped = daemon.usage("work").clients.unwrap();
  assert_eq!(grouped.len(), 1);
  assert_eq!(grouped[0].connected_attachments, 2);
  assert!(!grouped[0].legacy);
  assert_eq!(grouped[0].identity.client_name, "fixture-viewer");
  write_frame(&mut primary, &ClientMessage::Detach)
    .await
    .unwrap();
  drop(primary);
  drop(secondary);
  timeout(Duration::from_secs(3), async {
    loop {
      let row = daemon.usage("work");
      if row.unused() == Some(true)
        && row
          .clients
          .unwrap()
          .iter()
          .any(|c| c.reconnecting_attachments == 1)
      {
        break;
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .unwrap();
  assert!(daemon.unused().iter().any(|row| row.session.name == "work"));
  let stream = ctmux_ipc::connect_existing_daemon(&socket).await.unwrap();
  let (mut resumed, _) = resume_attach(
    stream,
    &identity,
    attached.attachment_token,
    viewer_request(&child),
  )
  .await
  .unwrap();
  let again = daemon.usage("work").clients.unwrap();
  assert_eq!(again.len(), 1);
  assert_eq!(again[0].identity, grouped[0].identity);
  assert_eq!(
    (
      again[0].connected_attachments,
      again[0].reconnecting_attachments
    ),
    (1, 0)
  );
  let promoted = daemon.view(&["promote", &child, "--name", "promoted"]);
  assert_eq!(daemon.usage("work").unused(), Some(true));
  assert_eq!(
    daemon.usage(&promoted.session_id).clients.unwrap()[0],
    again[0]
  );
  write_frame(&mut resumed, &ClientMessage::Detach)
    .await
    .unwrap();
}

fn viewer_request(id: &str) -> ctmux_client::AttachRequest {
  ctmux_client::AttachRequest {
    session: id.into(),
    resume_from: None,
    terminal_size: ctmux_proto::TerminalSize::default(),
    request_input_lease: false,
    request_layout_lease: false,
    request_command_line: false,
    request_running_command: false,
    presentation_window_bytes: ctmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES,
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tables_escape_presence_labels_but_json_preserves_client_information() {
  let daemon = Daemon::start().await;
  let name = "shared";
  daemon.success(&["new", "-ds", name, "--", "/bin/sh"]);
  let device = "测试\u{202e}device\u{2028}name";
  let peer = ctl_core::presence::ClientIdentity {
    instance_id: "id\u{2066}".into(),
    client_name: "viewer\u{202e}".into(),
    client_version: "v\u{2028}".into(),
    device_name: Some(device.into()),
  };
  let _modern = daemon.raw_viewer(name, "viewer", Some(peer.clone())).await;
  let _legacy = daemon
    .raw_viewer(name, "leg\x1bacy\tapp\u{202e}", None)
    .await;
  let text = daemon.success(&["clients", name]);
  assert_eq!(text.lines().count(), 3);
  assert!(
    text
      .lines()
      .skip(1)
      .all(|line| line.split('\t').count() == 7 && line.ends_with("\t1\t0"))
  );
  for escaped in [
    "测试\\u{202e}device\\u{2028}name",
    "viewer\\u{202e}",
    "v\\u{2028}",
    "id\\u{2066}",
    "legacyapp\\u{202e}",
  ] {
    assert!(text.contains(escaped), "missing escaped field {escaped}");
  }
  assert!(!text.contains(['\x1b', '\u{202e}', '\u{2028}', '\u{2066}']));
  let list = daemon.success(&["ls"]);
  assert!(list.contains("shared"));
  assert!(list.contains("2 clients"));
  let row = daemon.usage(name);
  assert_eq!(row.session.name, name);
  let clients = row.clients.unwrap();
  assert!(
    clients
      .iter()
      .any(|client| client.identity == peer && client.connected_attachments == 1)
  );
  assert!(clients.iter().any(|client| client.legacy
    && client.identity.client_name == "legacyapp\u{202e}"
    && client.connected_attachments == 1));
}

impl Daemon {
  async fn raw_viewer(
    &self,
    name: &str,
    app: &str,
    client_identity: Option<ctl_core::presence::ClientIdentity>,
  ) -> ctmux_ipc::Stream {
    use ctmux_proto::{ClientMessage, OpeningRequest, ServerMessage, read_frame, write_frame};
    let mut stream = ctmux_ipc::connect_existing_daemon(&self.directory.join("ctmux.sock"))
      .await
      .unwrap();
    ctmux_client::handshake(
      &mut stream,
      &ctmux_client::ClientIdentity {
        name: app.into(),
        version: "test".into(),
      },
    )
    .await
    .unwrap();
    write_frame(
      &mut stream,
      &OpeningRequest {
        client_identity,
        request: ClientMessage::AttachSession {
          session: name.into(),
          resume_from: None,
          terminal_size: ctmux_proto::TerminalSize::default(),
          request_input_lease: false,
          request_layout_lease: false,
          request_command_line: false,
          request_running_command: false,
          presentation_window_bytes: ctmux_proto::DEFAULT_PRESENTATION_WINDOW_BYTES,
        },
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      read_frame::<_, ServerMessage>(&mut stream).await.unwrap(),
      Some(ServerMessage::Attached { .. })
    ));
    stream
  }

  fn usage(&self, name: &str) -> ctmux_proto::SessionUsage {
    let json = self.success(&["clients", name, "--json"]);
    serde_json::from_str::<Vec<ctmux_proto::SessionUsage>>(&json)
      .unwrap()
      .remove(0)
  }

  fn unused(&self) -> Vec<ctmux_proto::SessionUsage> {
    serde_json::from_str::<Vec<ctmux_proto::SessionUsage>>(
      &self.success(&["ls", "--unused", "--json"]),
    )
    .unwrap()
  }

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
