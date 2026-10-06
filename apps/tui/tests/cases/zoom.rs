use crate::support::{Result, Screen, TestDaemon, Tui};
use ctmux_proto::{
  ClientMessage, CommandSpec, PaneGeometry, ServerMessage, SplitAxis, TerminalSize, ViewInfo,
};
use std::{fmt::Write as _, time::Duration};
use tokio::time::{Instant, sleep};

const COLUMNS: u16 = 120;
const ROWS: u16 = 25;

fn canvas(columns: u16, rows: u16) -> TerminalSize {
  TerminalSize {
    columns,
    rows: rows - 1,
    ..TerminalSize::default()
  }
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str)
}

fn shell_command(tag: &str) -> CommandSpec {
  CommandSpec {
    program: "/bin/sh".into(),
    arguments: vec![
      "-c".into(),
      // `stty size` reads the kernel's PTY dimensions, independently of the
      // daemon's layout metadata. Tags are arguments, never shell source.
      "PATH=/usr/bin:/bin; export PATH; stty -echo; printf '%s:ready\\n' \"$1\"; while IFS= read -r line; do case \"$line\" in size-*) printf '%s:%s:' \"$1\" \"$line\"; stty size;; *) printf '%s:%s\\n' \"$1\" \"$line\";; esac; done".into(),
      "ctmux-zoom-fixture".into(),
      tag.into(),
    ],
  }
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
  description: &str,
  predicate: impl Fn(&ViewInfo) -> bool,
) -> Result<ViewInfo> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let snapshot = view(daemon, session).await?;
    if predicate(&snapshot) {
      return Ok(snapshot);
    }
    if Instant::now() >= deadline {
      return Err(format!("waiting for {description}; last view: {snapshot:#?}").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn split_session(daemon: &TestDaemon, name: &str) -> Result<(String, String, String)> {
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some(name.into()),
      command: Some(shell_command("first")),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(COLUMNS, ROWS),
    })
    .await?
  else {
    return Err("expected new zoom session".into());
  };
  let ServerMessage::ViewSnapshot { view } = daemon
    .request(ClientMessage::SplitTerminal {
      terminal_id: session.terminal_id.clone(),
      axis: SplitAxis::Horizontal,
      command: Some(shell_command("second")),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(COLUMNS, ROWS),
    })
    .await?
  else {
    return Err("expected split zoom view".into());
  };
  let second = view
    .panes
    .iter()
    .find(|pane| pane.terminal_id != session.terminal_id)
    .ok_or("second zoom fixture pane was not created")?
    .terminal_id
    .clone();
  Ok((session.session_id, session.terminal_id, second))
}

async fn wait_split(tui: &mut Tui) -> Result<Screen> {
  tui
    .wait_screen("both panes rendered", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await
}

async fn wait_zoom(tui: &mut Tui) -> Result<Screen> {
  tui
    .wait_screen("first pane fills the shared zoomed view", |screen| {
      screen.contains("first:ready")
        && !screen.contains("second:ready")
        && footer(screen).contains("ZOOM")
    })
    .await
}

fn terminal_size(view: &ViewInfo, terminal_id: &str) -> TerminalSize {
  view
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id == terminal_id)
    .expect("fixture terminal belongs to view")
    .terminal_size
    .clone()
}

async fn check_shell_size(
  tui: &mut Tui,
  tag: &str,
  marker: &str,
  size: &TerminalSize,
) -> Result<()> {
  tui.send(format!("size-{marker}\r").as_bytes())?;
  let expected = format!("{tag}:size-{marker}:{} {}", size.rows, size.columns);
  tui
    .wait_screen("shell reports its actual PTY size", |screen| {
      screen.contains(&expected)
    })
    .await?;
  Ok(())
}

async fn detach(tui: &mut Tui) -> Result<()> {
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zoom_resizes_the_shell_and_restores_the_saved_split_layout() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_session(&daemon, "zoom-size").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  wait_split(&mut tui).await?;
  let original = view(&daemon, &session).await?;
  check_shell_size(
    &mut tui,
    "first",
    "split",
    &terminal_size(&original, &first),
  )
  .await?;

  tui.send(b"\x02z")?;
  wait_zoom(&mut tui).await?;
  let zoomed = wait_view(&daemon, &session, "server zoom", |snapshot| {
    snapshot.zoomed_terminal_id.as_deref() == Some(first.as_str())
  })
  .await?;
  assert_eq!(zoomed.layout, original.layout);
  assert_eq!(zoomed.panes, original.panes);
  assert_eq!(zoomed.terminals.len(), 2);
  assert_eq!(terminal_size(&zoomed, &first), zoomed.canvas_size);
  assert_eq!(
    terminal_size(&zoomed, &second),
    terminal_size(&original, &second)
  );
  check_shell_size(&mut tui, "first", "zoom", &zoomed.canvas_size).await?;

  tui.resize(100, 18)?;
  let resized = wait_view(&daemon, &session, "zoomed host resize", |snapshot| {
    snapshot.canvas_size == canvas(100, 18) && terminal_size(snapshot, &first) == canvas(100, 18)
  })
  .await?;
  assert_eq!(resized.layout, original.layout);
  tui
    .wait_screen("zoom status stays on the resized bottom row", |screen| {
      screen.rows.len() == 18 && footer(screen).contains("ZOOM")
    })
    .await?;
  check_shell_size(&mut tui, "first", "resized", &resized.canvas_size).await?;

  tui.send(b"\x02z")?;
  wait_split(&mut tui).await?;
  let restored = wait_view(&daemon, &session, "restored split", |snapshot| {
    snapshot.zoomed_terminal_id.is_none()
  })
  .await?;
  assert_eq!(restored.layout, original.layout);
  assert_eq!(restored.panes, resized.panes);
  assert_eq!(restored.terminals.len(), 2);
  check_shell_size(
    &mut tui,
    "first",
    "restored",
    &terminal_size(&restored, &first),
  )
  .await?;
  // The hidden attachment retains input ownership, and its original shell
  // continues running instead of being recreated on unzoom.
  tui.send(b"\x02\x1b[C")?;
  check_shell_size(
    &mut tui,
    "second",
    "still-running",
    &terminal_size(&restored, &second),
  )
  .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_tui_process_restores_the_server_owned_zoom() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = split_session(&daemon, "zoom-reconnect").await?;
  let mut original = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  wait_split(&mut original).await?;
  original.send(b"\x02z")?;
  wait_zoom(&mut original).await?;
  detach(&mut original).await?;
  drop(original);
  assert_eq!(
    view(&daemon, &session).await?.zoomed_terminal_id,
    Some(first.clone())
  );

  let mut reconnected = Tui::start(&daemon, &session, 100, 20).await?;
  wait_zoom(&mut reconnected).await?;
  let snapshot = wait_view(&daemon, &session, "reconnected zoom geometry", |snapshot| {
    snapshot.zoomed_terminal_id.as_deref() == Some(first.as_str())
      && snapshot.canvas_size == canvas(100, 20)
  })
  .await?;
  check_shell_size(
    &mut reconnected,
    "first",
    "reconnect",
    &snapshot.canvas_size,
  )
  .await?;
  reconnected.send(b"\x02z")?;
  wait_split(&mut reconnected).await?;
  detach(&mut reconnected).await?;
  drop(reconnected);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_zoom_rejects_a_client_without_resize_ownership() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = split_session(&daemon, "zoom-ownership").await?;
  let mut owner = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  wait_split(&mut owner).await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  observer
    .wait_screen("second client observes the owner", |screen| {
      screen.contains("second:ready")
        && footer(screen).contains("view only")
        && footer(screen).contains("shared size")
    })
    .await?;
  owner.send(b"\x02z")?;
  wait_zoom(&mut owner).await?;
  wait_zoom(&mut observer).await?;
  let zoomed = view(&daemon, &session).await?;
  assert_eq!(zoomed.zoomed_terminal_id.as_deref(), Some(first.as_str()));

  observer.send(b"\x02z")?;
  observer
    .wait_screen("zoom denial is visible to the nonowner", |screen| {
      footer(screen).contains("Resize lease required")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?, zoomed);
  check_shell_size(&mut owner, "first", "owner-remains", &zoomed.canvas_size).await?;
  owner.send(b"\x02z")?;
  wait_split(&mut owner).await?;
  wait_split(&mut observer).await?;
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut owner).await?;
  drop(owner);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ending_the_zoomed_pane_restores_the_surviving_shell() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = split_session(&daemon, "zoom-exit").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  wait_split(&mut tui).await?;
  tui.send(b"\x02z")?;
  wait_zoom(&mut tui).await?;
  assert!(matches!(
    daemon
      .request(ClientMessage::KillTerminal { terminal_id: first })
      .await?,
    ServerMessage::Success
  ));
  let remaining = wait_view(&daemon, &session, "removed zoomed pane", |snapshot| {
    snapshot.zoomed_terminal_id.is_none()
      && snapshot.panes.len() == 1
      && snapshot.panes[0].terminal_id == second
  })
  .await?;
  tui
    .wait_screen(
      "ended pane keeps its final screen until dismissal",
      |screen| {
        screen.contains("second:ready")
          && footer(screen).contains("press any key to close pane")
          && !footer(screen).contains("ZOOM")
      },
    )
    .await?;
  tui.send(b"\r")?;
  tui
    .wait_screen("surviving pane occupies the whole canvas", |screen| {
      screen.contains("second:ready")
        && !screen.contains("first:ready")
        && !footer(screen).contains("ZOOM")
    })
    .await?;
  assert_eq!(terminal_size(&remaining, &second), remaining.canvas_size);
  check_shell_size(&mut tui, "second", "survives", &remaining.canvas_size).await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

fn pane_rows(screen: &Screen, pane: &PaneGeometry) -> Vec<String> {
  screen.rows[usize::from(pane.top)..usize::from(pane.top + pane.rows)]
    .iter()
    .map(|row| {
      row
        .chars()
        .skip(usize::from(pane.left))
        .take(usize::from(pane.columns))
        .collect()
    })
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zoom_keeps_the_focused_panes_copy_mode_and_history() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = split_session(&daemon, "zoom-copy").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  wait_split(&mut tui).await?;
  let geometry = view(&daemon, &session)
    .await?
    .panes
    .into_iter()
    .find(|pane| pane.terminal_id == first)
    .ok_or("missing copy-mode fixture pane")?;
  let lines = (0..36).fold(String::new(), |mut lines, index| {
    writeln!(lines, "history-{index:02}").expect("write to string");
    lines
  });
  tui.send(format!("\x1b[200~{lines}\x1b[201~").as_bytes())?;
  tui
    .wait_screen("zoom fixture history created", |screen| {
      screen.contains("first:history-35")
    })
    .await?;
  tui.send(b"\x02[\x1b[5~\x1b[5~")?;
  let copied = tui
    .wait_screen("copy mode exposes older output", |screen| {
      footer(screen).contains("COPY") && screen.contains("first:history-00")
    })
    .await?;
  let frozen = pane_rows(&copied, &geometry);
  tui.send(b"\x02z")?;
  tui
    .wait_screen("zoom preserves copy mode", |screen| {
      footer(screen).contains("COPY")
        && screen.contains("first:history-00")
        && !screen.contains("second:ready")
    })
    .await?;
  wait_view(&daemon, &session, "zoomed copy view", |snapshot| {
    snapshot.zoomed_terminal_id.as_deref() == Some(first.as_str())
  })
  .await?;
  tui.send(b"\x02z")?;
  let restored = tui
    .wait_screen("unzoom restores the frozen split copy view", |screen| {
      footer(screen).contains("COPY")
        && screen.contains("first:history-00")
        && screen.contains("second:ready")
    })
    .await?;
  assert_eq!(pane_rows(&restored, &geometry), frozen);
  tui.send(b"q")?;
  tui
    .wait_screen(
      "leaving copy mode returns to preserved live output",
      |screen| !footer(screen).contains("COPY") && screen.contains("first:history-35"),
    )
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}
