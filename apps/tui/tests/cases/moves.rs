use crate::support::{Result, Screen, TestDaemon, TestProxy, Tui};
use ctmux_proto::{
  ClientMessage, PaneGeometry, ServerMessage, SessionInfo, SplitAxis, TerminalSize, ViewInfo,
  ViewLayout,
};
use portable_pty::CommandBuilder;
use std::time::Duration;
use tokio::time::{Instant, sleep};

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
    response => Err(format!("expected pane-move view, received {response:?}").into()),
  }
}

async fn wait_view(
  daemon: &TestDaemon,
  session: &str,
  description: &str,
  ready: impl Fn(&ViewInfo) -> bool,
) -> Result<ViewInfo> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let snapshot = view(daemon, session).await?;
    if ready(&snapshot) {
      return Ok(snapshot);
    }
    if Instant::now() >= deadline {
      return Err(format!("waiting for {description}; last view: {snapshot:#?}").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn sessions(daemon: &TestDaemon) -> Result<Vec<SessionInfo>> {
  match daemon.request(ClientMessage::ListSessions).await? {
    ServerMessage::SessionList { sessions } => Ok(sessions),
    response => Err(format!("expected pane-move sessions, received {response:?}").into()),
  }
}

async fn moved_session(daemon: &TestDaemon, terminal: &str) -> Result<SessionInfo> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let sessions = sessions(daemon).await?;
    if let Some(session) = sessions
      .iter()
      .find(|session| session.terminal_id == terminal)
    {
      return Ok(session.clone());
    }
    if Instant::now() >= deadline {
      return Err(format!("no flat session for moved terminal {terminal}: {sessions:#?}").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn split_fixture(daemon: &TestDaemon, name: &str) -> Result<(String, String, String)> {
  let session = daemon.create_echo_session(name, "first", canvas()).await?;
  let first = view(daemon, &session).await?.terminals[0]
    .terminal_id
    .clone();
  let split = daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", canvas())
    .await?;
  let second = split
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != first)
    .ok_or("second pane-move fixture was not created")?
    .terminal_id
    .clone();
  Ok((session, first, second))
}

fn leaf(terminal_id: &str) -> ViewLayout {
  ViewLayout::Terminal {
    terminal_id: terminal_id.into(),
  }
}

fn nested_layout(ids: [&str; 3], outer_weights: &[u32], inner_weights: &[u32]) -> ViewLayout {
  ViewLayout::Split {
    axis: SplitAxis::Horizontal,
    weights: outer_weights.into(),
    children: vec![
      ViewLayout::Split {
        axis: SplitAxis::Vertical,
        weights: inner_weights.into(),
        children: vec![leaf(ids[0]), leaf(ids[1])],
      },
      leaf(ids[2]),
    ],
  }
}

async fn nested_fixture(daemon: &TestDaemon) -> Result<(String, [String; 3])> {
  let (session, first, second) = split_fixture(daemon, "move-nested").await?;
  let split = daemon
    .split_echo(&first, SplitAxis::Vertical, "third", canvas())
    .await?;
  let third = split
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != first && terminal.terminal_id != second)
    .ok_or("third pane-move fixture was not created")?
    .terminal_id
    .clone();
  let layout = nested_layout([&first, &second, &third], &[], &[]);
  assert!(matches!(
    daemon
      .request(ClientMessage::UpdateView {
        session: session.clone(),
        expected_revision: split.revision,
        layout,
      })
      .await?,
    ServerMessage::ViewSnapshot { .. }
  ));
  Ok((session, [first, second, third]))
}

async fn prefix(tui: &mut Tui, key: &[u8]) -> Result<()> {
  tui.send(b"\x02")?;
  tui
    .wait_screen("pane-move prefix table is active", |screen| {
      footer(screen).contains("PREFIX")
    })
    .await?;
  tui.send(key)
}

async fn command(tui: &mut Tui, text: &str) -> Result<()> {
  tui.send(format!("\x02:{text}").as_bytes())?;
  let prompt = format!(":{text}");
  tui
    .wait_screen("pane-move command is ready to submit", |screen| {
      footer(screen) == prompt
    })
    .await?;
  tui.send(b"\r")
}

async fn detach(tui: &mut Tui) -> Result<()> {
  command(tui, "detach-client").await?;
  assert!(tui.wait_exit().await?.success());
  Ok(())
}

fn pane_contains(screen: &Screen, pane: &PaneGeometry, text: &str) -> bool {
  screen.rows[usize::from(pane.top)..usize::from(pane.top + pane.rows)]
    .iter()
    .any(|row| {
      row
        .chars()
        .skip(usize::from(pane.left))
        .take(usize::from(pane.columns))
        .collect::<String>()
        .contains(text)
    })
}

async fn ready_split(tui: &mut Tui) -> Result<()> {
  tui
    .wait_screen("both pane-move shells are ready", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await?;
  Ok(())
}

async fn input(tui: &mut Tui, tag: &str, marker: &str) -> Result<()> {
  tui.send(format!("{marker}\r").as_bytes())?;
  let expected = format!("{tag}:{marker}");
  tui
    .wait_screen("input follows the expected terminal identity", |screen| {
      screen.contains(&expected)
    })
    .await?;
  Ok(())
}

fn assert_slots(original: &ViewInfo, moved: &ViewInfo, ids: [&str; 3]) {
  assert_eq!(moved.layout, nested_layout(ids, &[71, 48], &[9, 14]));
  assert_eq!(moved.canvas_size, original.canvas_size);
  assert_eq!(moved.panes.len(), original.panes.len());
  for (slot, (before, after)) in original.panes.iter().zip(&moved.panes).enumerate() {
    assert_eq!(after.terminal_id, ids[slot]);
    assert_eq!(
      (after.left, after.top, after.columns, after.rows),
      (before.left, before.top, before.columns, before.rows),
      "swapping identities must preserve nested weighted slot geometry"
    );
    let terminal = moved
      .terminals
      .iter()
      .find(|terminal| terminal.terminal_id == ids[slot])
      .expect("moved terminal remains a view member");
    assert_eq!(terminal.terminal_size.columns, after.columns);
    assert_eq!(terminal.terminal_size.rows, after.rows);
  }
}

async fn visible_slots(tui: &mut Tui, snapshot: &ViewInfo, tags: &[(&str, &str)]) -> Result<()> {
  tui
    .wait_screen(
      "shared composition renders confirmed terminal slots",
      |screen| {
        snapshot.panes.iter().all(|pane| {
          let tag = tags
            .iter()
            .find(|(id, _)| *id == pane.terminal_id)
            .expect("fixture tag exists")
            .1;
          pane_contains(screen, pane, &format!("{tag}:ready"))
        })
      },
    )
    .await?;
  Ok(())
}

async fn weight_nested_fixture(
  daemon: &TestDaemon,
  session: &str,
  owner: &mut Tui,
  ids: [&str; 3],
  tags: &[(&str, &str)],
) -> Result<ViewInfo> {
  // Introduce weighted extents through the real owner's resize commands;
  // UpdateView cannot bypass the shared layout lease to size panes.
  command(owner, "resize-pane -R 11").await?;
  let horizontal = wait_view(daemon, session, "weighted outer split", |snapshot| {
    snapshot.panes[0].columns == 71
  })
  .await?;
  visible_slots(owner, &horizontal, tags).await?;
  command(owner, "resize-pane -U 3").await?;
  let weighted = nested_layout(ids, &[71, 48], &[9, 14]);
  let original = wait_view(daemon, session, "weighted nested split", |snapshot| {
    snapshot.layout == weighted
  })
  .await?;
  visible_slots(owner, &original, tags).await?;
  Ok(original)
}

async fn reconnect_panes(proxy: &TestProxy, tui: &mut Tui, pane_count: usize) -> Result<()> {
  let before = proxy.attachments();
  let stalled = proxy.stalled();
  proxy.interrupt();
  proxy.wait_stalled(stalled).await?;
  proxy.resume();
  proxy.wait_attachments(before + pane_count).await?;
  tui
    .wait_screen("moved panes recover their connections", |screen| {
      footer(screen).starts_with(" connected |")
    })
    .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn swaps_preserve_nested_weighted_slots_and_focus_policy_across_reconnect() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, [first, second, third]) = nested_fixture(&daemon).await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut owner = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  let tags = [
    (first.as_str(), "first"),
    (second.as_str(), "second"),
    (third.as_str(), "third"),
  ];
  let original = weight_nested_fixture(
    &daemon,
    &session,
    &mut owner,
    [&first, &second, &third],
    &tags,
  )
  .await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  visible_slots(&mut observer, &original, &tags).await?;

  for (key, text, ids, tag, marker) in [
    (
      Some(b"}".as_slice()),
      None,
      [&second, &first, &third],
      "first",
      "next-slot",
    ),
    (
      Some(b"{".as_slice()),
      None,
      [&first, &second, &third],
      "first",
      "previous-slot",
    ),
    (
      Some(b"{".as_slice()),
      None,
      [&third, &second, &first],
      "first",
      "wrapped-slot",
    ),
    (
      None,
      Some("swap-pane -D -d"),
      [&first, &second, &third],
      "third",
      "stay-next-slot",
    ),
    (
      None,
      Some("swap-pane -U -d"),
      [&first, &third, &second],
      "second",
      "stay-previous-slot",
    ),
  ] {
    let revision = view(&daemon, &session).await?.revision;
    if let Some(key) = key {
      prefix(&mut owner, key).await?;
    } else {
      command(&mut owner, text.expect("move command exists")).await?;
    }
    let expected = nested_layout(ids.map(String::as_str), &[71, 48], &[9, 14]);
    let moved = wait_view(&daemon, &session, "confirmed pane swap", |snapshot| {
      snapshot.revision > revision && snapshot.layout == expected
    })
    .await?;
    assert_slots(&original, &moved, ids.map(String::as_str));
    visible_slots(&mut owner, &moved, &tags).await?;
    visible_slots(&mut observer, &moved, &tags).await?;
    input(&mut owner, tag, marker).await?;
  }

  reconnect_panes(&proxy, &mut owner, original.panes.len()).await?;
  let recovered = view(&daemon, &session).await?;
  assert_slots(&original, &recovered, [&first, &third, &second]);
  visible_slots(&mut owner, &recovered, &tags).await?;
  prefix(&mut owner, b"}").await?;
  let expected = nested_layout([&second, &third, &first], &[71, 48], &[9, 14]);
  let after_reconnect = wait_view(
    &daemon,
    &session,
    "recovered owner can still swap panes",
    |view| view.revision > recovered.revision && view.layout == expected,
  )
  .await?;
  assert_slots(&original, &after_reconnect, [&second, &third, &first]);
  visible_slots(&mut owner, &after_reconnect, &tags).await?;
  visible_slots(&mut observer, &after_reconnect, &tags).await?;
  input(&mut owner, "second", "identity-after-reconnect").await?;
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut owner).await?;
  drop(owner);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_copy_selection_follows_its_terminal_through_swap_and_break() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_fixture(&daemon, "move-copy").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut tui).await?;
  tui.send(b"\x02[gvll")?;
  tui
    .wait_screen("original terminal has a frozen selection", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  prefix(&mut tui, b"}").await?;
  let moved = wait_view(&daemon, &session, "copying terminal changes slot", |view| {
    view.panes[1].terminal_id == first
  })
  .await?;
  let offset = usize::from(moved.panes[1].left);
  tui
    .wait_screen("copy selection moves with its pane", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (offset + 2, 0)
    })
    .await?;
  prefix(&mut tui, b"!").await?;
  let old = wait_view(
    &daemon,
    &session,
    "broken pane leaves its old session",
    |view| view.panes.len() == 1 && view.panes[0].terminal_id == second,
  )
  .await?;
  assert_eq!(old.layout, leaf(&second));
  let promoted = moved_session(&daemon, &first).await?;
  assert_ne!(promoted.session_id, session);
  let new = view(&daemon, &promoted.session_id).await?;
  assert_eq!(new.layout, leaf(&first));
  assert_eq!(new.terminals.len(), 1);
  tui
    .wait_screen(
      "followed flat session retains its frozen selection",
      |screen| {
        footer(screen).contains("COPY")
          && screen.cursor == (2, 0)
          && screen.contains("first:ready")
          && !screen.contains("second:ready")
      },
    )
    .await?;
  tui.send(b"y")?;
  tui
    .wait_screen("retained selection is copied after promotion", |screen| {
      footer(screen).contains("Copied to ctmux buffer")
    })
    .await?;
  command(&mut tui, "paste-buffer").await?;
  tui.send(b"\r")?;
  tui
    .wait_screen(
      "copied text reaches the original persistent shell",
      |screen| screen.contains("first:fir"),
    )
    .await?;
  input(&mut tui, "first", "identity-after-break").await?;
  let mut old_tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  old_tui
    .wait_screen(
      "followed break releases ownership in the old root",
      |screen| {
        screen.contains("second:ready")
          && footer(screen).contains("resize owner")
          && !screen.contains("first:ready")
      },
    )
    .await?;
  input(&mut old_tui, "second", "surviving-old-root").await?;
  detach(&mut old_tui).await?;
  drop(old_tui);
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detached_named_break_keeps_input_in_the_old_root_and_preserves_the_moved_shell()
-> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_fixture(&daemon, "move-detached").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut tui).await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut observer).await?;
  command(&mut tui, "select-pane -R").await?;
  input(&mut tui, "second", "before-detached-break").await?;
  command(&mut tui, "break-pane -d -n 'detached-shell'").await?;
  let old = wait_view(
    &daemon,
    &session,
    "detached break updates old root",
    |view| view.panes.len() == 1 && view.panes[0].terminal_id == first,
  )
  .await?;
  assert_eq!(old.layout, leaf(&first));
  assert_eq!(old.session_name, "move-detached");
  let promoted = moved_session(&daemon, &second).await?;
  assert_ne!(promoted.session_id, session);
  assert_eq!(promoted.name, "detached-shell");
  let new = view(&daemon, &promoted.session_id).await?;
  assert_eq!(new.layout, leaf(&second));
  assert_eq!(new.terminals.len(), 1);
  assert_eq!(sessions(&daemon).await?.len(), 2);
  tui
    .wait_screen("detached break leaves the old root selected", |screen| {
      screen.contains("first:ready") && !screen.contains("second:ready")
    })
    .await?;
  let survivor_marker =
    "surviving-pane-expands-across-the-former-divider-and-keeps-accepting-input";
  input(&mut tui, "first", survivor_marker).await?;
  let survivor_output = format!("first:{survivor_marker}");
  observer
    .wait_screen(
      "source observer removes the live promoted pane without a tombstone",
      |screen| {
        screen.contains(&survivor_output)
          && !screen.contains("second:ready")
          && !screen.contains("Terminal no longer exists")
          && !screen.contains("press a key")
          && !footer(screen).contains("ended")
          && !footer(screen).contains("press any key")
          && footer(screen).starts_with(" connected |")
      },
    )
    .await?;
  assert_eq!(old.panes[0].columns, COLUMNS);
  assert_eq!(old.panes[0].rows, ROWS - 1);
  let mut new_tui = Tui::start(&daemon, &promoted.session_id, COLUMNS, ROWS).await?;
  new_tui
    .wait_screen("moved shell keeps output from before promotion", |screen| {
      screen.contains("second:before-detached-break")
    })
    .await?;
  input(&mut new_tui, "second", "new-root-after-break").await?;
  detach(&mut new_tui).await?;
  drop(new_tui);
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_move_replies_keep_live_input_help_resize_and_detach_responsive() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_fixture(&daemon, "move-delayed").await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  ready_split(&mut tui).await?;
  proxy.hold_move_results();
  command(&mut tui, "swap-pane -D -d").await?;
  proxy.wait_held_move_result(0).await?;
  wait_view(
    &daemon,
    &session,
    "server swaps before its reply is released",
    |view| view.panes[0].terminal_id == second && view.panes[1].terminal_id == first,
  )
  .await?;
  input(&mut tui, "first", "live-input-during-move").await?;
  prefix(&mut tui, b"?").await?;
  tui
    .wait_screen("local help opens while the move reply is held", |screen| {
      screen.contains("Commands after Ctrl+b")
    })
    .await?;
  tui.send(b"\x1b")?;
  tui
    .wait_screen(
      "Escape closes help while the move reply is held",
      |screen| {
        !screen.contains("Commands after Ctrl+b") && screen.contains("first:live-input-during-move")
      },
    )
    .await?;
  tui.resize(100, 18)?;
  tui
    .wait_screen(
      "host resize redraws while the move reply is held",
      |screen| {
        screen.columns == 100
          && screen.rows.len() == 18
          && screen.contains("first:live-input-during-move")
      },
    )
    .await?;
  let resized = wait_view(
    &daemon,
    &session,
    "shared canvas resize during pending move",
    |view| view.canvas_size.columns == 100 && view.canvas_size.rows == 17,
  )
  .await?;
  proxy.release_move_results();
  tui
    .wait_screen(
      "delayed confirmation applies detached focus in the original slot",
      |screen| screen.cursor.0 < 50 && footer(screen).contains("pane 1"),
    )
    .await?;
  input(&mut tui, "second", "live-input-after-reply").await?;
  assert_eq!(view(&daemon, &session).await?.panes, resized.panes);

  let held = proxy.held_move_results();
  proxy.hold_move_results();
  prefix(&mut tui, b"{").await?;
  proxy.wait_held_move_result(held).await?;
  wait_view(
    &daemon,
    &session,
    "second move is pending before detach",
    |view| view.panes[0].terminal_id == first && view.panes[1].terminal_id == second,
  )
  .await?;
  detach(&mut tui).await?;
  proxy.release_move_results();
  drop(tui);
  drop(proxy);
  assert_eq!(sessions(&daemon).await?.len(), 1);
  daemon.shutdown().await?;
  Ok(())
}

async fn read_only_tui(daemon: &TestDaemon, session: &str) -> Result<Tui> {
  let program = option_env!("CARGO_BIN_EXE_ctmux-tui")
    .ok_or("ctmux-tui binary is required for read-only pane-move launcher")?;
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

async fn reject_moves(
  tui: &mut Tui,
  daemon: &TestDaemon,
  session: &str,
  message: &str,
  original: &ViewInfo,
) -> Result<()> {
  for (key, text) in [
    (Some(b"{".as_slice()), None),
    (None, Some("swap-pane -D -d")),
    (Some(b"!".as_slice()), None),
    (None, Some("break-pane -d -n denied")),
  ] {
    if let Some(key) = key {
      prefix(tui, key).await?;
    } else {
      command(tui, text.expect("rejected move command exists")).await?;
    }
    tui
      .wait_screen(
        "pane move is rejected with explicit authority error",
        |screen| footer(screen).contains(message),
      )
      .await?;
    let unchanged = view(daemon, session).await?;
    assert_eq!(unchanged.layout, original.layout);
    assert_eq!(unchanged.panes, original.panes);
    assert_eq!(unchanged.revision, original.revision);
    assert_eq!(sessions(daemon).await?.len(), 1);
  }
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_moves_reject_read_only_and_foreign_resize_ownership_without_mutation() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_fixture(&daemon, "move-ownership").await?;
  let mut owner = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut owner).await?;
  owner
    .wait_screen("first client owns shared layout", |screen| {
      footer(screen).contains("resize owner")
    })
    .await?;
  let original = view(&daemon, &session).await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  ready_split(&mut observer).await?;
  reject_moves(
    &mut observer,
    &daemon,
    &session,
    "Resize lease required",
    &original,
  )
  .await?;
  let mut read_only = read_only_tui(&daemon, &session).await?;
  reject_moves(
    &mut read_only,
    &daemon,
    &session,
    "This attachment is read only",
    &original,
  )
  .await?;
  prefix(&mut owner, b"}").await?;
  wait_view(
    &daemon,
    &session,
    "owner can still rearrange panes",
    |view| view.panes[0].terminal_id == second && view.panes[1].terminal_id == first,
  )
  .await?;
  input(&mut owner, "first", "owner-after-rejections").await?;
  detach(&mut read_only).await?;
  drop(read_only);
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut owner).await?;
  drop(owner);
  daemon.shutdown().await?;
  Ok(())
}
