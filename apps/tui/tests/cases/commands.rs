use crate::support::{Result, Screen, TestDaemon, TestProxy, Tui};
use ctmux_proto::{
  ClientMessage, CommandSpec, PaneGeometry, ServerMessage, SplitAxis, TerminalSize, ViewInfo,
};
use portable_pty::CommandBuilder;
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout};

const COLUMNS: u16 = 120;
const ROWS: u16 = 25;

fn canvas() -> TerminalSize {
  TerminalSize {
    columns: COLUMNS,
    rows: ROWS - 1,
    ..TerminalSize::default()
  }
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str).trim_end()
}

async fn view(daemon: &TestDaemon, session: &str) -> Result<ViewInfo> {
  match daemon
    .request(ClientMessage::GetView {
      session: session.into(),
    })
    .await?
  {
    ServerMessage::ViewSnapshot { view } => Ok(view),
    response => Err(format!("expected view snapshot, got {response:?}").into()),
  }
}

async fn wait_view(
  daemon: &TestDaemon,
  session: &str,
  ready: impl Fn(&ViewInfo) -> bool,
) -> Result<ViewInfo> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let current = view(daemon, session).await?;
    if ready(&current) {
      return Ok(current);
    }
    if Instant::now() >= deadline {
      return Err(format!("command not applied: {current:#?}").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

fn pane<'a>(view: &'a ViewInfo, id: &str) -> &'a PaneGeometry {
  view
    .panes
    .iter()
    .find(|pane| pane.terminal_id == id)
    .expect("fixture pane belongs to view")
}

async fn prompt(tui: &mut Tui, text: &str) -> Result<Screen> {
  tui.send(format!("\x02:{text}").as_bytes())?;
  let expected = format!(":{text}");
  tui
    .wait_screen("local command prompt", |screen| footer(screen) == expected)
    .await
}

fn command(tui: &mut Tui, text: &str) -> Result<()> {
  tui.send(format!("\x02:{text}\r").as_bytes())
}

async fn detach(tui: &mut Tui) -> Result<()> {
  command(tui, "detach-client")?;
  assert!(tui.wait_exit().await?.success());
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_prompt_edits_unicode_and_paste_locally_on_the_fixed_bottom_row() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("prompt-input", "first", canvas())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("shell ready", |screen| screen.contains("first:ready"))
    .await?;
  let initial = prompt(&mut tui, "界ab").await?;
  assert_eq!(initial.cursor, (5, usize::from(ROWS - 1)));
  tui.send(b"\x1b[D\x7f\x1b[H\x1b[C")?;
  tui.send("é".as_bytes())?;
  tui.send(b"\x1b[F\x7f")?;
  tui
    .wait_screen("Unicode editing uses character boundaries", |screen| {
      footer(screen) == ":界é" && screen.cursor == (4, usize::from(ROWS - 1))
    })
    .await?;
  tui.resize(100, 18)?;
  tui
    .wait_screen("prompt follows host resize", |screen| {
      screen.rows.len() == 18 && footer(screen) == ":界é" && screen.cursor == (4, 17)
    })
    .await?;
  tui.send(b"\x1b[200~split-window -h\n\x1b[201~")?;
  tui
    .wait_screen("pasted newline does not submit a command", |screen| {
      footer(screen).starts_with(':') && footer(screen).contains("split-window -h")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?.panes.len(), 1);
  tui.send(b"\x03after-cancel\r")?;
  let returned = tui
    .wait_screen("cancel returns to ordinary shell input", |screen| {
      screen.contains("first:after-cancel")
    })
    .await?;
  assert!(!returned.contains("first:界"));
  assert!(!returned.contains("first:split-window"));
  assert_eq!(view(&daemon, &session).await?.panes.len(), 1);
  command(&mut tui, "refresh-client")?;
  tui
    .wait_screen("submitted prompt closes", |screen| {
      !footer(screen).starts_with(':')
    })
    .await?;
  prompt(&mut tui, "").await?;
  tui.send(b"\x1b[A")?;
  tui
    .wait_screen("Up recalls a submitted command", |screen| {
      footer(screen) == ":refresh-client"
    })
    .await?;
  tui.send(b"\x1b[Bdetach-cl\t")?;
  tui
    .wait_screen(
      "Down restores the draft and Tab completes a command name",
      |screen| footer(screen) == ":detach-client",
    )
    .await?;
  tui.send(b"\x1b")?;
  tui
    .wait_screen("Escape cancels completion", |screen| {
      !footer(screen).starts_with(':')
    })
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

fn size_shell(tag: &str) -> CommandSpec {
  CommandSpec {
    program: "/bin/sh".into(),
    arguments: vec!["-c".into(), "PATH=/usr/bin:/bin; export PATH; stty -echo; printf '%s:ready\\n' \"$1\"; while IFS= read -r line; do case \"$line\" in size-*) printf '%s:%s:' \"$1\" \"$line\"; stty size;; *) printf '%s:%s\\n' \"$1\" \"$line\";; esac; done".into(), "ctmux-command-fixture".into(), tag.into()],
  }
}

async fn split_fixture(daemon: &TestDaemon, name: &str) -> Result<(String, String, String)> {
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some(name.into()),
      command: Some(size_shell("first")),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(),
    })
    .await?
  else {
    return Err("expected created session".into());
  };
  let split = daemon
    .request(ClientMessage::SplitTerminal {
      terminal_id: session.terminal_id.clone(),
      axis: SplitAxis::Horizontal,
      command: Some(size_shell("second")),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(),
    })
    .await?;
  let ServerMessage::ViewSnapshot { view } = split else {
    return Err("expected split view".into());
  };
  let second = view
    .panes
    .iter()
    .find(|pane| pane.terminal_id != session.terminal_id)
    .ok_or("missing second fixture pane")?
    .terminal_id
    .clone();
  Ok((session.session_id, session.terminal_id, second))
}

async fn actual_size(tui: &mut Tui, tag: &str, marker: &str, size: &TerminalSize) -> Result<()> {
  tui.send(format!("size-{marker}\r").as_bytes())?;
  let expected = format!("{tag}:size-{marker}:{} {}", size.rows, size.columns);
  tui
    .wait_screen("focused shell confirms kernel PTY size", |screen| {
      screen.contains(&expected)
    })
    .await?;
  Ok(())
}

fn terminal_size<'a>(view: &'a ViewInfo, id: &str) -> &'a TerminalSize {
  &view
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id == id)
    .expect("fixture terminal exists")
    .terminal_size
}

async fn ready_split(tui: &mut Tui) -> Result<()> {
  tui
    .wait_screen("both controlled shells ready", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_prompt_preserves_frozen_copy_selection_and_restores_ordinary_input() -> Result<()>
{
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("prompt-copy", "first", canvas())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("copy fixture ready", |screen| {
      screen.contains("first:ready")
    })
    .await?;
  tui.send(b"\x02[gvll")?;
  let frozen = tui
    .wait_screen("three characters selected in copy mode", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  prompt(&mut tui, "not-submitted").await?;
  tui.send(b"\x1b")?;
  let cancelled = tui
    .wait_screen("cancel returns to frozen copy selection", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  assert_eq!(
    cancelled.rows[..usize::from(ROWS - 1)],
    frozen.rows[..usize::from(ROWS - 1)]
  );
  command(&mut tui, "refresh-client")?;
  tui
    .wait_screen("submitted local action retains copy selection", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  tui.send(b"y")?;
  tui
    .wait_screen("selection copied", |screen| {
      footer(screen).contains("Copied to ctmux buffer")
    })
    .await?;
  command(&mut tui, "paste-buffer")?;
  tui.send(b"\r")?;
  tui
    .wait_screen(
      "retained selection is pasted into the original pane",
      |screen| screen.contains("first:fir"),
    )
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_commands_confirm_focus_geometry_kernel_sizes_and_kill_confirmation() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_fixture(&daemon, "prompt-panes").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut tui).await?;
  command(&mut tui, "select-pane -R")?;
  tui.send(b"selected-by-command\r")?;
  tui
    .wait_screen("select-pane changes input focus", |screen| {
      footer(screen).contains("pane 2") && screen.contains("second:selected-by-command")
    })
    .await?;
  let original = view(&daemon, &session).await?;
  command(&mut tui, "resize-pane -L 3")?;
  let resized = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&original, &first).columns - 3
  })
  .await?;
  actual_size(
    &mut tui,
    "second",
    "resized",
    terminal_size(&resized, &second),
  )
  .await?;
  command(&mut tui, "resize-pane -Z")?;
  let zoomed = wait_view(&daemon, &session, |view| {
    view.zoomed_terminal_id.as_deref() == Some(&second)
  })
  .await?;
  actual_size(
    &mut tui,
    "second",
    "zoomed",
    terminal_size(&zoomed, &second),
  )
  .await?;
  command(&mut tui, "resize-pane -Z")?;
  let restored = wait_view(&daemon, &session, |view| view.zoomed_terminal_id.is_none()).await?;
  assert_eq!(restored.layout, resized.layout);
  actual_size(
    &mut tui,
    "second",
    "restored",
    terminal_size(&restored, &second),
  )
  .await?;
  command(&mut tui, "split-window -v")?;
  let split = wait_view(&daemon, &session, |view| view.panes.len() == 3).await?;
  let created = split
    .panes
    .iter()
    .find(|pane| pane.terminal_id != first && pane.terminal_id != second)
    .ok_or("command split did not create a pane")?
    .terminal_id
    .clone();
  assert_eq!(pane(&split, &first), pane(&restored, &first));
  command(&mut tui, "kill-pane")?;
  tui
    .wait_screen("kill-pane requests the existing confirmation", |screen| {
      screen.contains("Terminate active pane? y confirms")
    })
    .await?;
  assert!(
    view(&daemon, &session)
      .await?
      .panes
      .iter()
      .any(|pane| pane.terminal_id == created)
  );
  tui.send(b"n")?;
  tui
    .wait_screen("cancelling kill preserves the pane", |screen| {
      !screen.contains("Terminate active pane? y confirms")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?.panes.len(), 3);
  command(&mut tui, "kill-pane")?;
  tui
    .wait_screen("second kill confirmation", |screen| {
      screen.contains("Terminate active pane? y confirms")
    })
    .await?;
  tui.send(b"y")?;
  wait_view(&daemon, &session, |view| view.panes.len() == 2).await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

async fn wait_named_session(daemon: &TestDaemon, name: &str) -> Result<String> {
  timeout(Duration::from_secs(5), async {
    loop {
      let ServerMessage::SessionList { sessions } =
        daemon.request(ClientMessage::ListSessions).await?
      else {
        return Err("expected session list".into());
      };
      if let Some(session) = sessions.iter().find(|session| session.name == name) {
        return Ok(session.session_id.clone());
      }
      sleep(Duration::from_millis(10)).await;
    }
  })
  .await?
}

async fn ready_named_session(tui: &mut Tui, name: &str) -> Result<()> {
  let expected = format!("[{name}]");
  tui
    .wait_screen("named session connected with input ownership", |screen| {
      let status = footer(screen);
      status.starts_with(" connected | history ready |")
        && status.contains(&expected)
        && status.contains(" | input |")
    })
    .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_commands_use_flat_named_sessions_and_prompt_detach_exits_cleanly() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let alpha = daemon
    .create_echo_session("prompt-alpha", "alpha", canvas())
    .await?;
  daemon
    .create_echo_session("prompt-beta", "beta", canvas())
    .await?;
  let mut tui = Tui::start(&daemon, &alpha, COLUMNS, ROWS).await?;
  command(&mut tui, "switch-client -t 'prompt-beta'")?;
  ready_named_session(&mut tui, "prompt-beta").await?;
  tui.send(b"targeted-session\r")?;
  tui
    .wait_screen("quoted target selects the flat beta session", |screen| {
      screen.contains("beta:targeted-session")
    })
    .await?;
  command(&mut tui, "switch-client -p")?;
  ready_named_session(&mut tui, "prompt-alpha").await?;
  command(&mut tui, "switch-client -n")?;
  ready_named_session(&mut tui, "prompt-beta").await?;
  command(&mut tui, "list-sessions")?;
  tui
    .wait_screen("list-sessions shows both named sessions", |screen| {
      screen.contains("prompt-alpha") && screen.contains("prompt-beta")
    })
    .await?;
  tui.send(b"\x1b")?;
  tui
    .wait_screen("session list closes before creating a session", |screen| {
      !screen.contains("Sessions —")
    })
    .await?;
  prompt(&mut tui, "new-session -s 'prompt-created'").await?;
  tui.send(b"\r")?;
  let created = wait_named_session(&daemon, "prompt-created")
    .await
    .map_err(|error| format!("new-session failed: {error}; {}", tui.screen().diagnostic()))?;
  ready_named_session(&mut tui, "prompt-created").await?;
  assert_ne!(created, alpha);
  command(&mut tui, "switch-client -t 'prompt-alpha'")?;
  ready_named_session(&mut tui, "prompt-alpha").await?;
  tui.send(b"returned-to-fixture\r")?;
  tui
    .wait_screen(
      "switching back preserves the original persistent shell",
      |screen| screen.contains("alpha:returned-to-fixture"),
    )
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  // Cooperative shutdown also reaps the default shell created by new-session.
  daemon.shutdown().await?;
  Ok(())
}

async fn read_only_tui(daemon: &TestDaemon, session: &str) -> Result<Tui> {
  let program = option_env!("CARGO_BIN_EXE_ctmux-tui").ok_or("missing TUI binary")?;
  let mut command = CommandBuilder::new(program);
  command.arg("--socket");
  command.arg(&daemon.socket);
  command.args(["--read-only", session]);
  command.env_clear();
  command.env("HOME", daemon.directory.join("home"));
  command.env("PATH", "/usr/bin:/bin");
  command.env("SHELL", "/bin/sh");
  command.env("TERM", "xterm-256color");
  command.env("LANG", "C.UTF-8");
  command.cwd(&daemon.directory);
  let mut tui = Tui::spawn(command, COLUMNS, ROWS)?;
  ready_split(&mut tui).await?;
  Ok(tui)
}

async fn lease_commands_are_idempotent(tui: &mut Tui) -> Result<()> {
  for (text, status) in [
    ("release-input", "view only"),
    ("release-resize", "shared size"),
    ("take-input", "input"),
    ("take-resize", "resize owner"),
  ] {
    command(tui, text)?;
    tui
      .wait_screen("explicit ownership request applied", |screen| {
        footer(screen).contains(status)
      })
      .await?;
    command(tui, text)?;
    command(tui, "refresh-client")?;
    tui
      .wait_screen(
        "repeating ownership command preserves its requested state",
        |screen| !footer(screen).starts_with(':') && footer(screen).contains(status),
      )
      .await?;
  }
  Ok(())
}

async fn reject_unsafe_commands(tui: &mut Tui, daemon: &TestDaemon, session: &str) -> Result<()> {
  let original = view(daemon, session).await?;
  for (text, error) in [
    ("not-a-command", "Unknown command"),
    ("resize-pane -R -L", "Resize cells must be an integer"),
    ("split-window -h -v", "Unsupported arguments"),
    ("refresh-client; detach-client", "command chains"),
  ] {
    prompt(tui, text).await?;
    tui.send(b"\r")?;
    tui
      .wait_screen("invalid command has a local error", |screen| {
        !footer(screen).starts_with(':') && footer(screen).contains(error)
      })
      .await?;
    let unchanged = view(daemon, session).await?;
    assert_eq!(unchanged.revision, original.revision);
    assert_eq!(unchanged.panes, original.panes);
  }
  tui.send(b"after-rejections\r")?;
  let shown = tui
    .wait_screen(
      "rejection restores shell input without executing command text",
      |screen| screen.contains("first:after-rejections"),
    )
    .await?;
  assert!(!shown.contains("first:not-a-command"));
  assert!(!shown.contains("first:resize-pane"));
  assert!(!shown.contains("first:refresh-client"));
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_commands_and_explicit_leases_preserve_foreign_and_read_only_authority()
-> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = split_fixture(&daemon, "prompt-ownership").await?;
  let mut owner = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut owner).await?;
  reject_unsafe_commands(&mut owner, &daemon, &session).await?;
  lease_commands_are_idempotent(&mut owner).await?;
  let original = view(&daemon, &session).await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut observer).await?;
  command(&mut observer, "take-input")?;
  command(&mut observer, "take-resize")?;
  command(&mut observer, "resize-pane -R 3")?;
  observer
    .wait_screen("foreign resize ownership refuses mutation", |screen| {
      footer(screen).contains("Resize lease required")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?.panes, original.panes);
  let mut read_only = read_only_tui(&daemon, &session).await?;
  command(&mut read_only, "resize-pane -R 3")?;
  read_only
    .wait_screen("read-only command is rejected", |screen| {
      footer(screen).contains("This attachment is read only")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?.panes, original.panes);
  command(&mut owner, "resize-pane -R 1")?;
  let changed = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&original, &first).columns + 1
  })
  .await?;
  actual_size(
    &mut owner,
    "first",
    "still-owner",
    terminal_size(&changed, &first),
  )
  .await?;
  detach(&mut read_only).await?;
  drop(read_only);
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut owner).await?;
  drop(owner);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_prompt_editing_and_cancellation_remain_responsive_during_reconnect() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("prompt-reconnect", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("reconnect fixture ready", |screen| {
      screen.contains("first:ready")
    })
    .await?;
  proxy.stall_requests();
  proxy.wait_stalled(0).await?;
  timeout(Duration::from_secs(1), prompt(&mut tui, "local-draft")).await??;
  tui.send(b"\x1b")?;
  timeout(
    Duration::from_secs(1),
    tui.wait_screen("cancel responds while refresh is stalled", |screen| {
      !footer(screen).starts_with(':') && footer(screen).starts_with(" connected |")
    }),
  )
  .await??;
  prompt(&mut tui, "offline").await?;
  let before = proxy.attachments();
  let stalled = proxy.stalled();
  proxy.interrupt();
  proxy.wait_stalled(stalled).await?;
  tui.send(b"\x7f")?;
  timeout(
    Duration::from_secs(1),
    tui.wait_screen(
      "editing stays local through transport interruption",
      |screen| footer(screen) == ":offlin",
    ),
  )
  .await??;
  tui.send(b"\x03")?;
  timeout(
    Duration::from_secs(1),
    tui.wait_screen("cancel exposes reconnect state", |screen| {
      footer(screen).starts_with(" reconnecting |")
    }),
  )
  .await??;
  command(&mut tui, "list-keys")?;
  timeout(
    Duration::from_secs(1),
    tui.wait_screen("local command help opens while disconnected", |screen| {
      screen.contains("Commands after Ctrl+b")
    }),
  )
  .await??;
  tui.send(b"\x1b")?;
  proxy.resume();
  proxy.wait_attachments(before + 1).await?;
  tui
    .wait_screen("live terminal recovers", |screen| {
      footer(screen).starts_with(" connected |") && screen.contains("first:ready")
    })
    .await?;
  tui.send(b"after-command-reconnect\r")?;
  tui
    .wait_screen(
      "input returns after prompt cancellation and reconnect",
      |screen| screen.contains("first:after-command-reconnect"),
    )
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}
