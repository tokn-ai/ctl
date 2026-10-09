use crate::support::{Result, Screen, TestDaemon, Tui};
use ctmux_proto::{
  ClientMessage, PaneGeometry, ServerMessage, SessionInfo, SessionStatus, SplitAxis, TerminalSize,
  ViewInfo,
};
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
    response => Err(format!("expected navigation view, received {response:?}").into()),
  }
}

async fn split_session(daemon: &TestDaemon, name: &str) -> Result<(String, ViewInfo)> {
  let session = daemon.create_echo_session(name, "first", canvas()).await?;
  let first = view(daemon, &session).await?.terminals[0]
    .terminal_id
    .clone();
  let split = daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", canvas())
    .await?;
  Ok((session, split))
}

async fn prefix(tui: &mut Tui, key: &[u8]) -> Result<()> {
  tui.send(b"\x02")?;
  tui
    .wait_screen("navigation prefix table is active", |screen| {
      footer(screen).contains("PREFIX")
    })
    .await?;
  tui.send(key)
}

async fn command(tui: &mut Tui, text: &str) -> Result<()> {
  tui.send(format!("\x02:{text}").as_bytes())?;
  let prompt = format!(":{text}");
  tui
    .wait_screen("navigation command is ready to submit", |screen| {
      footer(screen) == prompt
    })
    .await?;
  tui.send(b"\r")
}

async fn input(tui: &mut Tui, tag: &str, marker: &str) -> Result<()> {
  let ready = format!("{tag}:ready");
  tui
    .wait_screen("selected navigation shell owns live input", |screen| {
      screen.contains(&ready)
        && footer(screen).starts_with(" connected |")
        && footer(screen).contains(" | input |")
    })
    .await?;
  input_response(tui, tag, marker).await
}

async fn input_response(tui: &mut Tui, tag: &str, marker: &str) -> Result<()> {
  tui.send(format!("{marker}\r").as_bytes())?;
  let expected = format!("{tag}:{marker}");
  tui
    .wait_screen("navigation routes input to the selected shell", |screen| {
      screen.contains(&expected)
    })
    .await?;
  Ok(())
}

async fn detach(tui: &mut Tui) -> Result<()> {
  command(tui, "detach-client").await?;
  assert!(tui.wait_exit().await?.success());
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

fn labels_visible(screen: &Screen, snapshot: &ViewInfo) -> bool {
  snapshot.panes.iter().enumerate().all(|(index, pane)| {
    let label = (index + 1).to_string();
    pane_rows(screen, pane)
      .iter()
      .any(|row| row.contains(&label))
  })
}

async fn remove_session(daemon: &TestDaemon, session: &str) -> Result<()> {
  let terminal_id = view(daemon, session).await?.terminals[0]
    .terminal_id
    .clone();
  assert!(matches!(
    daemon
      .request(ClientMessage::KillTerminal { terminal_id })
      .await?,
    ServerMessage::Success
  ));
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let ServerMessage::SessionList { sessions } =
      daemon.request(ClientMessage::ListSessions).await?
    else {
      return Err("expected navigation session list".into());
    };
    if !sessions
      .iter()
      .any(|info| info.session_id == session && info.status == SessionStatus::Running)
    {
      return Ok(());
    }
    if Instant::now() >= deadline {
      return Err(format!("removed navigation session remains live: {sessions:?}").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn ordered_sessions(daemon: &TestDaemon) -> Result<Vec<SessionInfo>> {
  let ServerMessage::SessionList { mut sessions } =
    daemon.request(ClientMessage::ListSessions).await?
  else {
    return Err("expected navigation session list".into());
  };
  sessions.sort_by_key(|session| (session.created_at_ms, session.session_id.clone()));
  Ok(sessions)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_numbers_and_last_pane_preserve_frozen_copy_and_input_identity() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, split) = split_session(&daemon, "nav-panes").await?;
  let second = split.panes[1].terminal_id.clone();
  let snapshot = daemon
    .split_echo(&second, SplitAxis::Horizontal, "third", canvas())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("all navigation shells are ready", |screen| {
      screen.contains("first:ready")
        && screen.contains("second:ready")
        && screen.contains("third:ready")
    })
    .await?;
  tui.send(b"\x02[gvll")?;
  let selected = tui
    .wait_screen("first pane has a frozen copy selection", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  let frozen = pane_rows(&selected, &snapshot.panes[0]);
  prefix(&mut tui, b"q").await?;
  tui
    .wait_screen("pane numbers appear within their actual slots", |screen| {
      labels_visible(screen, &snapshot)
    })
    .await?;
  tui.send(b"3digit-route\r")?;
  tui
    .wait_screen("label digit is consumed before shell input", |screen| {
      screen.contains("third:digit-route") && !labels_visible(screen, &snapshot)
    })
    .await?;
  assert!(!tui.screen().contains("third:3digit-route"));
  prefix(&mut tui, b";").await?;
  let returned = tui
    .wait_screen("last-pane restores the frozen selection", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  assert_eq!(pane_rows(&returned, &snapshot.panes[0]), frozen);
  command(&mut tui, "select-pane -l").await?;
  input(&mut tui, "third", "select-last").await?;
  command(&mut tui, "select-pane -t 2").await?;
  input(&mut tui, "second", "number-target").await?;
  command(&mut tui, "last-pane").await?;
  input(&mut tui, "third", "command-last").await?;
  command(&mut tui, "select-pane -t 1").await?;
  tui
    .wait_screen("direct selection retains first pane copy", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (2, 0)
    })
    .await?;
  tui.send(b"y")?;
  tui
    .wait_screen("retained selection reaches the copy buffer", |screen| {
      footer(screen).contains("Copied to ctmux buffer")
    })
    .await?;
  command(&mut tui, "paste-buffer").await?;
  tui.send(b"\r")?;
  tui
    .wait_screen("copy belongs to its original shell", |screen| {
      screen.contains("first:fir")
    })
    .await?;
  command(&mut tui, "display-panes").await?;
  tui
    .wait_screen("display-panes prompt opens labels", |screen| {
      labels_visible(screen, &snapshot)
    })
    .await?;
  tui.send(b"\x1b")?;
  input(&mut tui, "first", "cancelled-labels").await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_session_restores_its_focused_pane_and_survives_a_removed_target() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (alpha, _) = split_session(&daemon, "nav-alpha").await?;
  let beta = daemon
    .create_echo_session("nav-beta", "beta", canvas())
    .await?;
  let mut tui = Tui::start(&daemon, &alpha, COLUMNS, ROWS).await?;
  tui
    .wait_screen("initial session panes are ready", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await?;
  command(&mut tui, "select-pane -t 2").await?;
  input(&mut tui, "second", "remembered-focus").await?;
  command(&mut tui, "switch-client -t nav-beta").await?;
  input(&mut tui, "beta", "second-session").await?;
  prefix(&mut tui, b"L").await?;
  input(&mut tui, "second", "uppercase-last-session").await?;
  prefix(&mut tui, b"l").await?;
  input(&mut tui, "beta", "lowercase-last-session").await?;
  command(&mut tui, "switch-client -l").await?;
  input(&mut tui, "second", "command-last-session").await?;
  remove_session(&daemon, &beta).await?;
  prefix(&mut tui, b"L").await?;
  tui
    .wait_screen("removed last session is rejected locally", |screen| {
      footer(screen).contains("Session not found:")
        && screen.contains("first:ready")
        && screen.contains("second:ready")
    })
    .await?;
  input_response(&mut tui, "second", "removed-target-keeps-current").await?;
  assert!(tui.screen().contains("first:ready"));
  assert!(!tui.screen().contains("beta:ready"));
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_picker_preserves_selected_identity_when_an_earlier_row_disappears() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  daemon
    .create_echo_session("pick-alpha", "alpha", canvas())
    .await?;
  daemon
    .create_echo_session("pick-beta", "beta", canvas())
    .await?;
  daemon
    .create_echo_session("pick-gamma", "gamma", canvas())
    .await?;
  let sessions = ordered_sessions(&daemon).await?;
  assert_eq!(sessions.len(), 3);
  let [earlier, selected, active] = [&sessions[0], &sessions[1], &sessions[2]];
  let active_ready = format!("{}:ready", active.name.trim_start_matches("pick-"));
  let highlight = format!("> {}", selected.name);
  let mut tui = Tui::start(&daemon, &active.session_id, COLUMNS, ROWS).await?;
  tui
    .wait_screen(
      "picker starts from the surviving active session",
      |screen| screen.contains(&active_ready),
    )
    .await?;
  prefix(&mut tui, b"s").await?;
  tui
    .wait_screen(
      "picker initially highlights the current session",
      |screen| screen.contains(&format!("> {}", active.name)),
    )
    .await?;
  tui.send(b"\x1b[A")?;
  tui
    .wait_screen(
      "picker highlights its middle session by identity",
      |screen| screen.contains(&highlight) && screen.contains(&earlier.name),
    )
    .await?;
  remove_session(&daemon, &earlier.session_id).await?;
  tui
    .wait_screen(
      "picker refresh retains the same highlighted session",
      |screen| {
        screen.contains(&highlight)
          && screen.contains(&active.name)
          && !screen.contains(&earlier.name)
      },
    )
    .await?;
  tui.send(b"\r")?;
  input(
    &mut tui,
    selected.name.trim_start_matches("pick-"),
    "picker-identity",
  )
  .await?;
  assert!(!tui.screen().contains(&active_ready));
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}
