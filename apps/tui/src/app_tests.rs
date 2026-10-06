use super::*;
use crate::actions::Action;
use crate::keys::KeyState;
use ctmux_proto::CommandSpec;
#[path = "../tests/support/daemon.rs"]
// Process tests also use this fixture's echo and explicit shutdown helpers.
#[allow(dead_code)]
mod daemon;
use daemon::TestDaemon as Daemon;

struct RelayTransport {
  socket: PathBuf,
  connections: std::sync::atomic::AtomicUsize,
}

impl crate::Transport for RelayTransport {
  fn connect(&self) -> crate::ConnectFuture<'_> {
    Box::pin(async move {
      let mut daemon = tokio::net::UnixStream::connect(&self.socket).await?;
      self
        .connections
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      let (client, mut relay) = tokio::io::duplex(4096);
      tokio::spawn(async move {
        let _ = tokio::io::copy_bidirectional(&mut daemon, &mut relay).await;
      });
      Ok(Box::new(client) as crate::Stream)
    })
  }

  fn archive_key(&self) -> String {
    "remote-fixture".into()
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transported_shell_scrolls_frozen_history_and_reconnects() -> Result<()> {
  use crossterm::event::{MouseEvent, MouseEventKind};
  use std::fmt::Write as _;
  let daemon = Daemon::start().await?;
  let transport = RelayTransport {
    socket: daemon.directory.join("ctmux.sock"),
    connections: std::sync::atomic::AtomicUsize::new(0),
  };
  // Every operation must use the supplied transport, never this absent socket.
  let mut app = App::new(
    daemon.directory.join("unused.sock"),
    false,
    input::parse_prefix("Ctrl+b")?,
  );
  app.transport = Some(&transport);
  app.size = (80, 10);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let primary = app.focused.clone();
  app.panes[&primary]
    .control
    .input(
      (0..40)
        .fold(String::new(), |mut text, row| {
          writeln!(text, "line-{row}").unwrap();
          text
        })
        .into_bytes(),
    )
    .await?;
  wait_for_text(&mut app, &primary, "echo:line-39").await?;
  let wheel = |kind| {
    Event::Mouse(MouseEvent {
      kind,
      column: 1,
      row: 1,
      modifiers: KeyModifiers::NONE,
    })
  };
  app.event(wheel(MouseEventKind::ScrollUp)).await?;
  let mode = app.active_copy().unwrap();
  assert_eq!(
    mode.top,
    mode.lines.len().saturating_sub(9).saturating_sub(5)
  );
  let snapshot = mode.lines.clone();
  let top = mode.top;
  app.panes[&primary]
    .control
    .input(b"AFTER_SCROLL\n".to_vec())
    .await?;
  wait_for_text(&mut app, &primary, "echo:AFTER_SCROLL").await?;
  assert_eq!(app.active_copy().unwrap().lines, snapshot);
  assert_eq!(app.active_copy().unwrap().top, top);
  app.event(wheel(MouseEventKind::ScrollDown)).await?;
  assert!(app.active_copy().is_none());
  app
    .event(Event::Key(KeyEvent::new(
      KeyCode::PageUp,
      KeyModifiers::SHIFT,
    )))
    .await?;
  assert!(app.active_copy().is_some());
  app
    .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    .await?;
  app.panes[&primary].control.detach().await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&primary].connected {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  assert!(app.status().contains("reconnecting"));
  app.refresh().await?;
  assert!(app.panes[&primary].connected);
  assert!(
    transport
      .connections
      .load(std::sync::atomic::Ordering::Relaxed)
      > 3
  );
  assert_eq!(app.archive_key(), "remote-fixture");
  app.detach().await;
  Ok(())
}

impl Daemon {
  fn app(&self, read_only: bool) -> App<'_> {
    let mut app = App::new(
      self.directory.join("ctmux.sock"),
      read_only,
      input::parse_prefix("Ctrl+b").unwrap(),
    );
    app.size = (80, 25);
    app
  }
}

async fn create_shell(app: &App<'_>) -> Result<String> {
  let ServerMessage::SessionCreated { session } = app
    .request(ClientMessage::CreateSession {
      name: None,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "while IFS= read -r line; do printf 'echo:%s\n' \"$line\"; done".into(),
        ],
      }),
      working_directory: None,
      terminal_size: app.canvas_size(),
    })
    .await?
  else {
    return Err("expected session".into());
  };
  Ok(session.session_id)
}

async fn wait_for_text(app: &mut App<'_>, id: &str, text: &str) -> Result<()> {
  timeout(Duration::from_secs(5), async {
    loop {
      app.drain().await;
      if app.panes[id]
        .model
        .vt
        .lines()
        .map(avt::Line::text)
        .collect::<Vec<_>>()
        .join("\n")
        .contains(text)
      {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_canvas_input_viewer_reconnect_and_detach() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut owner = daemon.app(false);
  let session = create_shell(&owner).await?;
  owner.start(Some(session.clone())).await?;
  let primary = owner.focused.clone();
  owner.panes[&primary]
    .control
    .input(b"PRIMARY_MARK\n".to_vec())
    .await?;
  wait_for_text(&mut owner, &primary, "echo:PRIMARY_MARK").await?;
  assert_copy_mode(&mut owner, &primary).await?;

  owner.split(SplitAxis::Horizontal).await?;
  assert_eq!(owner.panes.len(), 2);
  let child = owner.focused.clone();
  assert_ne!(primary, child);
  owner.panes[&child]
    .control
    .input(b"printf 'CHILD_MARK\\n'\n".to_vec())
    .await?;
  wait_for_text(&mut owner, &child, "CHILD_MARK").await?;
  assert!(
    !owner.panes[&primary]
      .model
      .vt
      .text()
      .join("\n")
      .contains("CHILD_MARK")
  );

  assert_tmux_shortcuts(&mut owner).await?;
  assert_viewer_does_not_resize(&daemon, &session).await?;
  assert_lease_handoff(&daemon, &mut owner, &session).await?;
  owner.size = (100, 31);
  owner.resize().await?;
  wait_for_canvas(&mut owner, 100, 30).await?;
  owner.focus(crate::actions::Direction::Left);
  assert_eq!(owner.focused, primary);
  assert_pane_sizes(&mut owner).await?;

  // Resume through a fresh checkpoint, preserving the logical attachment lease.
  owner.panes.get_mut(&primary).unwrap().connected = false;
  owner.reconcile().await?;
  wait_for_text(&mut owner, &primary, "PRIMARY_MARK").await?;
  assert!(
    owner.panes[&primary]
      .control
      .state()
      .leases()
      .input
      .owned_by_client
  );

  owner.detach().await;
  let mut reattached = daemon.app(false);
  reattached.start(Some(session.clone())).await?;
  assert_eq!(reattached.panes.len(), 2);
  assert!(
    reattached
      .panes
      .values()
      .all(|pane| pane.control.state().leases().input.owned_by_client)
  );
  reattached.detach().await;
  owner
    .request(ClientMessage::KillSession { session })
    .await?;
  Ok(())
}

async fn assert_viewer_does_not_resize(daemon: &Daemon, session: &str) -> Result<()> {
  let mut viewer = daemon.app(true);
  viewer.size = (40, 12);
  viewer.start(Some(session.into())).await?;
  assert_eq!(viewer.view.as_ref().unwrap().canvas_size.columns, 80);
  assert!(viewer.panes.values().all(|pane| {
    let leases = pane.control.state().leases();
    !leases.input.owned_by_client && !leases.layout.owned_by_client
  }));
  viewer.size = (20, 8);
  viewer.resize().await?;
  viewer.refresh_view().await?;
  assert_eq!(viewer.view.as_ref().unwrap().canvas_size.columns, 80);
  viewer.detach().await;
  Ok(())
}

async fn wait_for_canvas(app: &mut App<'_>, columns: u16, rows: u16) -> Result<()> {
  timeout(Duration::from_secs(5), async {
    loop {
      app.drain().await;
      app.refresh_view().await?;
      let size = &app.view.as_ref().unwrap().canvas_size;
      if size.columns == columns && size.rows == rows {
        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?
}

async fn assert_pane_sizes(app: &mut App<'_>) -> Result<()> {
  timeout(Duration::from_secs(5), async {
    loop {
      app.drain().await;
      if app.view.as_ref().unwrap().panes.iter().all(|rect| {
        app.panes[&rect.terminal_id].model.vt.size()
          == (usize::from(rect.columns), usize::from(rect.rows))
      }) {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  Ok(())
}

async fn wait_for_lease(app: &mut App<'_>, lease: LeaseKind, expected: bool) -> Result<()> {
  timeout(Duration::from_secs(5), async {
    loop {
      app.drain().await;
      let leases = app.panes[&app.focused].control.state().leases();
      let owned = match lease {
        LeaseKind::Input => leases.input.owned_by_client,
        LeaseKind::Layout => leases.layout.owned_by_client,
      };
      if owned == expected {
        break;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  Ok(())
}

async fn assert_lease_handoff(daemon: &Daemon, owner: &mut App<'_>, session: &str) -> Result<()> {
  let mut other = daemon.app(false);
  other.size = (60, 20);
  other.start(Some(session.into())).await?;
  other.focused.clone_from(&owner.focused);
  assert!(
    !other.panes[&other.focused]
      .control
      .state()
      .leases()
      .input
      .owned_by_client
  );
  owner.toggle_lease(LeaseKind::Input).await?;
  wait_for_lease(owner, LeaseKind::Input, false).await?;
  other.toggle_lease(LeaseKind::Input).await?;
  wait_for_lease(&mut other, LeaseKind::Input, true).await?;

  owner.toggle_lease(LeaseKind::Layout).await?;
  timeout(Duration::from_secs(5), async {
    while owner
      .panes
      .values()
      .any(|pane| pane.control.state().leases().layout.owned_by_client)
    {
      owner.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  other.toggle_lease(LeaseKind::Layout).await?;
  wait_for_lease(&mut other, LeaseKind::Layout, true).await?;
  wait_for_canvas(&mut other, 60, 19).await?;
  other.detach().await;

  owner.toggle_lease(LeaseKind::Input).await?;
  wait_for_lease(owner, LeaseKind::Input, true).await?;
  owner.toggle_lease(LeaseKind::Layout).await?;
  wait_for_lease(owner, LeaseKind::Layout, true).await?;
  wait_for_canvas(owner, 80, 24).await?;
  Ok(())
}

async fn assert_tmux_shortcuts(app: &mut App<'_>) -> Result<()> {
  let focused = app.focused.clone();
  let count = app.panes.len();
  app.select(&focused).await?;
  assert_eq!(app.focused, focused);
  app.execute(Action::Sessions).await?;
  assert!(matches!(app.overlay, Overlay::Sessions(_)));
  assert_eq!(app.panes.len(), count);
  app.overlay = Overlay::None;
  app.execute(Action::NextPane).await?;
  assert_ne!(app.focused, focused);
  app.execute(Action::NextPane).await?;
  assert_eq!(app.focused, focused);
  let rect = app
    .view
    .as_ref()
    .unwrap()
    .panes
    .iter()
    .find(|pane| pane.terminal_id != focused)
    .unwrap()
    .clone();
  app
    .event(Event::Mouse(crossterm::event::MouseEvent {
      kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
      column: rect.left,
      row: rect.top,
      modifiers: KeyModifiers::NONE,
    }))
    .await?;
  assert_eq!(app.focused, rect.terminal_id);
  app
    .event(Event::Mouse(MouseEvent {
      kind: MouseEventKind::Down(MouseButton::Left),
      column: 0,
      row: app.size.1 - 1,
      modifiers: KeyModifiers::NONE,
    }))
    .await?;
  assert_eq!(app.focused, rect.terminal_id);
  app.focused = focused;
  let owner = app
    .panes
    .values()
    .any(|pane| pane.control.state().leases().layout.owned_by_client);
  app.execute(Action::Refresh).await?;
  assert_eq!(
    app
      .panes
      .values()
      .any(|pane| pane.control.state().leases().layout.owned_by_client),
    owner
  );
  app.execute(Action::History { page_back: true }).await?;
  assert!(app.active_copy().is_some());
  app
    .key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    .await?;
  assert!(app.active_copy().is_none());
  Ok(())
}

async fn assert_copy_mode(app: &mut App<'_>, primary: &str) -> Result<()> {
  app.execute(Action::History { page_back: false }).await?;
  let snapshot = app.active_copy().unwrap().lines.clone();
  app.event(Event::Paste("IGNORED_PASTE\n".into())).await?;
  app.panes[primary]
    .control
    .input(b"DURING_COPY\n".to_vec())
    .await?;
  wait_for_text(app, primary, "echo:DURING_COPY").await?;
  assert_eq!(app.active_copy().unwrap().lines, snapshot);
  assert!(
    !app.panes[primary]
      .model
      .copy_lines()
      .join("\n")
      .contains("IGNORED_PASTE")
  );
  app
    .key(KeyEvent::new(
      KeyCode::Esc,
      crossterm::event::KeyModifiers::NONE,
    ))
    .await?;
  assert!(app.active_copy().is_none());
  app.copy_buffer = Some("BUFFER_PASTE\n".into());
  app.execute(Action::Paste).await?;
  wait_for_text(app, primary, "echo:BUFFER_PASTE").await?;
  Ok(())
}

async fn split_exit_shell(app: &mut App<'_>) -> Result<String> {
  // This fixture tests dismissal, not the runner's login shell startup. Wait
  // for a known program to be ready before asking it to produce final output.
  let ServerMessage::ViewSnapshot { view } = app
    .request(ClientMessage::SplitTerminal {
      terminal_id: app.focused.clone(),
      axis: SplitAxis::Horizontal,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "printf 'CHILD_READY\\n'; IFS= read -r line; printf 'FINAL_CHILD\\n'; exit 7".into(),
        ],
      }),
      working_directory: None,
      terminal_size: app.canvas_size(),
    })
    .await?
  else {
    return Err("expected split view".into());
  };
  let child = view
    .panes
    .iter()
    .find(|pane| !app.panes.contains_key(&pane.terminal_id))
    .ok_or("split did not create a pane")?
    .terminal_id
    .clone();
  app.refresh_view().await?;
  app.focused.clone_from(&child);
  wait_for_text(app, &child, "CHILD_READY").await?;
  Ok(child)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ended_panes_and_confirmed_missing_sessions_wait_for_dismissal() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let primary = app.focused.clone();
  let child = split_exit_shell(&mut app).await?;
  app.panes[&child]
    .control
    .input(b"finish\n".to_vec())
    .await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&child].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .map_err(|_| {
    format!(
      "child did not end: connected={}, notice={}",
      app.panes[&child].connected, app.message
    )
  })?;
  app.refresh().await?;
  assert_eq!(app.panes.len(), 2);
  assert!(
    app.panes[&child]
      .model
      .copy_lines()
      .join("\n")
      .contains("FINAL_CHILD")
  );
  assert!(app.status().contains("code 7"));
  assert!(
    !app
      .key(KeyEvent::new(
        KeyCode::Char('z'),
        crossterm::event::KeyModifiers::NONE
      ))
      .await?
  );
  assert_eq!(app.panes.len(), 1);
  assert_eq!(app.focused, primary);
  let archives = app.local_archives()?;
  assert!(archives.iter().any(|archive| {
    archive
      .terminals
      .iter()
      .any(|pane| pane.lines.join("\n").contains("FINAL_CHILD"))
  }));

  let mut missing = daemon.app(true);
  missing.start(Some("confirmed-absent".into())).await?;
  assert!(missing.status().contains("no longer exists"));
  assert!(
    missing
      .key(KeyEvent::new(
        KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE
      ))
      .await?
  );

  app.request(ClientMessage::KillSession { session }).await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&primary].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .map_err(|_| {
    format!(
      "killed primary did not end: connected={}, notice={}",
      app.panes[&primary].connected, app.message
    )
  })?;
  app.refresh().await?;
  assert!(app.ended.is_some());
  assert_eq!(app.panes.len(), 1);
  assert!(
    app
      .key(KeyEvent::new(
        KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE
      ))
      .await?
  );
  app.detach().await;
  Ok(())
}

#[tokio::test]
async fn archived_output_opens_without_a_daemon() -> Result<()> {
  let directory = std::env::temp_dir().join(format!("rtui-archive-{}", uuid::Uuid::new_v4()));
  let socket = directory.join("absent.sock");
  let mut app = App::new(socket.clone(), true, input::parse_prefix("Ctrl+b")?);
  app.selected_id = "missing".into();
  app.ended = Some("Session no longer exists".into());
  app.save_archive()?;
  app.open_archive("missing")?;
  assert!(app.archive_only);
  assert!(!socket.exists());
  assert!(matches!(app.overlay, Overlay::ArchiveTerminals(..)));
  std::fs::remove_dir_all(directory)?;
  Ok(())
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
  Event::Mouse(MouseEvent {
    kind,
    column,
    row,
    modifiers: KeyModifiers::NONE,
  })
}

async fn split_program(app: &mut App<'_>, program: &str) -> Result<String> {
  let ServerMessage::ViewSnapshot { view } = app
    .request(ClientMessage::SplitTerminal {
      terminal_id: app.focused.clone(),
      axis: SplitAxis::Horizontal,
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), program.into()],
      }),
      working_directory: None,
      terminal_size: app.canvas_size(),
    })
    .await?
  else {
    return Err("expected split".into());
  };
  let child = view
    .panes
    .iter()
    .find(|pane| !app.panes.contains_key(&pane.terminal_id))
    .unwrap()
    .terminal_id
    .clone();
  app.refresh_view().await?;
  Ok(child)
}

async fn press(app: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
  app.event(Event::Key(KeyEvent::new(code, modifiers))).await
}

async fn prefix(app: &mut App<'_>) -> Result<()> {
  assert!(!press(app, KeyCode::Char('b'), KeyModifiers::CONTROL).await?);
  assert!(matches!(app.keys, KeyState::Prefix));
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modified_prefix_bindings_do_not_detach_or_change_pane_focus() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let child = split_program(
    &mut app,
    "printf 'CHILD_READY\\n'; while IFS= read -r line; do printf 'child:%s\\n' \"$line\"; done",
  )
  .await?;
  wait_for_text(&mut app, &child, "CHILD_READY").await?;

  for (code, modifiers) in [
    (KeyCode::Char('d'), KeyModifiers::CONTROL),
    (KeyCode::Right, KeyModifiers::CONTROL),
    (KeyCode::Right, KeyModifiers::ALT),
    (KeyCode::Right, KeyModifiers::SHIFT),
    (KeyCode::PageUp, KeyModifiers::SHIFT),
  ] {
    prefix(&mut app).await?;
    assert!(!press(&mut app, code, modifiers).await?);
    assert_eq!(app.focused, primary);
    assert!(matches!(app.keys, KeyState::Root));
    assert!(app.copies.is_empty());
  }

  press(&mut app, KeyCode::PageUp, KeyModifiers::SHIFT).await?;
  assert!(app.copies.contains_key(&primary));
  assert!(matches!(app.keys, KeyState::Root));
  press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await?;
  assert!(app.copies.is_empty());

  app
    .event(Event::Key(KeyEvent::new_with_kind(
      KeyCode::Char('b'),
      KeyModifiers::CONTROL,
      KeyEventKind::Release,
    )))
    .await?;
  assert!(matches!(app.keys, KeyState::Root));
  prefix(&mut app).await?;
  app
    .event(Event::Key(KeyEvent::new_with_kind(
      KeyCode::Right,
      KeyModifiers::NONE,
      KeyEventKind::Release,
    )))
    .await?;
  assert!(matches!(app.keys, KeyState::Prefix));
  assert_eq!(app.focused, primary);
  press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await?;

  // Terminals may report uppercase letters as a lowercase code plus SHIFT.
  prefix(&mut app).await?;
  press(&mut app, KeyCode::Char('a'), KeyModifiers::SHIFT).await?;
  assert!(matches!(app.overlay, Overlay::Archives(_)));
  assert!(matches!(app.keys, KeyState::Root));
  press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await?;
  prefix(&mut app).await?;
  press(&mut app, KeyCode::Char('?'), KeyModifiers::SHIFT).await?;
  assert!(matches!(app.overlay, Overlay::Help));
  press(&mut app, KeyCode::Esc, KeyModifiers::NONE).await?;
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_pane_navigation_falls_through_to_input_and_can_be_reprefixed() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let echo_child =
    "printf 'CHILD_READY\\n'; while IFS= read -r line; do printf 'child:%s\\n' \"$line\"; done";
  let child = split_program(&mut app, echo_child).await?;
  app.focused.clone_from(&child);
  let nested = split_program(&mut app, echo_child).await?;
  wait_for_text(&mut app, &nested, "CHILD_READY").await?;
  app.focused.clone_from(&primary);

  prefix(&mut app).await?;
  press(&mut app, KeyCode::Right, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, child);
  assert!(matches!(app.keys, KeyState::Repeat { .. }));
  app
    .event(Event::Key(KeyEvent::new_with_kind(
      KeyCode::Right,
      KeyModifiers::NONE,
      KeyEventKind::Repeat,
    )))
    .await?;
  assert_eq!(app.focused, nested);
  assert!(matches!(app.keys, KeyState::Repeat { .. }));

  // Commands without a fresh prefix become ordinary input during repetition.
  for ch in ['d', 'o', 'a'] {
    app.keys = KeyState::Repeat {
      until: Instant::now() + Duration::from_secs(1),
    };
    assert!(!press(&mut app, KeyCode::Char(ch), KeyModifiers::NONE).await?);
    assert_eq!(app.focused, nested);
    assert!(matches!(app.keys, KeyState::Root));
  }
  press(&mut app, KeyCode::Enter, KeyModifiers::NONE).await?;
  wait_for_text(&mut app, &nested, "child:doa").await?;

  app.keys = KeyState::Repeat {
    until: Instant::now() + Duration::from_secs(1),
  };
  prefix(&mut app).await?;
  assert!(press(&mut app, KeyCode::Char('d'), KeyModifiers::NONE).await?);
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_navigation_precedes_copy_keys_until_the_deadline_expires() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let child = split_program(
    &mut app,
    "printf 'CHILD_READY\\n'; while IFS= read -r line; do printf 'child:%s\\n' \"$line\"; done",
  )
  .await?;
  wait_for_text(&mut app, &child, "CHILD_READY").await?;
  app
    .copies
    .insert(child.clone(), CopyMode::new(vec!["abcdef".into()]));

  prefix(&mut app).await?;
  press(&mut app, KeyCode::Right, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, child);
  assert_eq!(app.active_copy().unwrap().cursor.column, 0);
  press(&mut app, KeyCode::Left, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, primary);
  press(&mut app, KeyCode::Right, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, child);
  assert_eq!(app.active_copy().unwrap().cursor.column, 0);

  app.keys = KeyState::Repeat {
    until: Instant::now() - Duration::from_millis(1),
  };
  press(&mut app, KeyCode::Right, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, child);
  assert_eq!(app.active_copy().unwrap().cursor.column, 1);
  assert!(matches!(app.keys, KeyState::Root));

  // A new prefix still works while a pane is in copy mode.
  prefix(&mut app).await?;
  press(&mut app, KeyCode::Left, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, primary);
  assert!(app.copies.contains_key(&child));

  let rect = app
    .view
    .as_ref()
    .unwrap()
    .panes
    .iter()
    .find(|rect| rect.terminal_id == child)
    .unwrap()
    .clone();
  app
    .event(mouse(MouseEventKind::Moved, rect.left + 1, rect.top))
    .await?;
  assert!(matches!(app.keys, KeyState::Repeat { .. }));
  assert_eq!(app.focused, primary);
  app
    .event(mouse(MouseEventKind::ScrollUp, rect.left + 1, rect.top))
    .await?;
  assert!(matches!(app.keys, KeyState::Root));
  assert_eq!(app.focused, child);
  assert_eq!(app.active_copy().unwrap().cursor.column, 1);
  press(&mut app, KeyCode::Left, KeyModifiers::NONE).await?;
  assert_eq!(app.focused, child);
  assert_eq!(app.active_copy().unwrap().cursor.column, 0);
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copy_stays_in_its_pane_while_other_panes_update_and_accept_input() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  app.size = (80, 10);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let primary = app.focused.clone();
  let child = split_program(
    &mut app,
    "printf 'CHILD_READY\\n'; while IFS= read -r line; do printf 'child:%s\\n' \"$line\"; done",
  )
  .await?;
  wait_for_text(&mut app, &child, "CHILD_READY").await?;
  app.panes[&primary]
    .control
    .input(b"BEFORE_COPY\n".to_vec())
    .await?;
  wait_for_text(&mut app, &primary, "echo:BEFORE_COPY").await?;
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&primary].lines.clone();
  app.panes[&primary]
    .control
    .input(b"AFTER_COPY\n".to_vec())
    .await?;
  app.panes[&child]
    .control
    .input(b"LIVE_UPDATE\n".to_vec())
    .await?;
  wait_for_text(&mut app, &primary, "echo:AFTER_COPY").await?;
  wait_for_text(&mut app, &child, "child:LIVE_UPDATE").await?;
  let frame = app.frame().text_rows().join("\n");
  assert!(frame.contains("echo:BEFORE_COPY"));
  assert!(!frame.contains("echo:AFTER_COPY"));
  assert!(frame.contains("child:LIVE_UPDATE"));
  assert!(frame.contains("COPY"));
  app
    .key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
    .await?;
  app
    .key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))
    .await?;
  assert_eq!(app.focused, child);
  assert!(app.active_copy().is_none());
  app.event(Event::Paste("LIVE_INPUT\n".into())).await?;
  assert!(matches!(app.keys, KeyState::Root));
  wait_for_text(&mut app, &child, "child:LIVE_INPUT").await?;
  app.execute(Action::History { page_back: false }).await?;
  assert_eq!(app.copies.len(), 2);
  app
    .key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL))
    .await?;
  app
    .key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
    .await?;
  assert_eq!(app.copies[&primary].lines, frozen);
  app.panes[&primary].control.detach().await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&primary].connected {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  app.refresh().await?;
  assert!(app.panes[&primary].connected);
  assert_eq!(app.copies[&primary].lines, frozen);
  app
    .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
    .await?;
  assert!(!app.copies.contains_key(&primary));
  assert!(app.copies.contains_key(&child));
  app.select(&session).await?;
  assert!(app.copies.is_empty());
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mouse_routes_application_reports_and_local_drags_to_the_original_pane() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  app.size = (80, 10);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let child = split_program(&mut app, "stty -echo -icanon; printf 'HIDDEN_PRIMARY\\033[?1049h\\033[2J\\033[H\\033[?1002;1006hMOUSE_READY\\n'; dd bs=1 count=9 2>/dev/null | od -An -tx1; dd bs=1 count=9 2>/dev/null | od -An -tx1; printf 'REPORTS_DONE\\n'; sleep 30").await?;
  wait_for_text(&mut app, &child, "MOUSE_READY").await?;
  let rect = app
    .view
    .as_ref()
    .unwrap()
    .panes
    .iter()
    .find(|rect| rect.terminal_id == child)
    .unwrap()
    .clone();
  // Pane-local (2,1) becomes the application's one-based (3,2).
  app
    .event(mouse(
      MouseEventKind::Down(MouseButton::Left),
      rect.left + 2,
      rect.top + 1,
    ))
    .await?;
  assert!(!app.copies.contains_key(&child));
  // Keep delivery to the original pane, clamping a release over the left neighbour.
  app.keys = KeyState::Prefix;
  app
    .event(mouse(
      MouseEventKind::Up(MouseButton::Left),
      0,
      app.size.1 - 1,
    ))
    .await?;
  app.keys = KeyState::Root;
  wait_for_text(&mut app, &child, "REPORTS_DONE").await?;
  let received = app.panes[&child]
    .model
    .vt
    .lines()
    .map(avt::Line::text)
    .collect::<Vec<_>>()
    .join("\n")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ");
  assert!(
    received.contains("1b 5b 3c 30 3b 33 3b 32 4d"),
    "{received}"
  );
  assert!(
    received.contains("1b 5b 3c 30 3b 31 3b 39 6d"),
    "{received}"
  );
  assert!(app.mouse_capture.is_none());
  assert_alternate_screen_drag(&mut app, &child, rect.left).await?;
  // Read-only attachments can browse history but must never send application input.
  let mut viewer = daemon.app(true);
  viewer.size = app.size;
  viewer.start(Some(child.clone())).await?;
  wait_for_text(&mut viewer, &child, "REPORTS_DONE").await?;
  assert!(viewer.panes[&child].model.input_modes.mouse().enabled());
  viewer
    .event(mouse(MouseEventKind::ScrollUp, rect.left + 1, 1))
    .await?;
  assert!(viewer.copies.contains_key(&child));
  viewer.detach().await;
  app.focused = primary.clone();
  app.execute(Action::History { page_back: false }).await?;
  // Use known frozen text to verify cross-border drag release without forwarding.
  app
    .copies
    .insert(primary.clone(), CopyMode::new(vec!["abcdef".into()]));
  app
    .event(mouse(MouseEventKind::Down(MouseButton::Left), 1, 0))
    .await?;
  app
    .event(mouse(MouseEventKind::Drag(MouseButton::Left), 3, 0))
    .await?;
  app
    .event(mouse(
      MouseEventKind::Up(MouseButton::Left),
      rect.left + 1,
      0,
    ))
    .await?;
  assert_eq!(app.copy_buffer.as_deref(), Some("bcdef"));
  assert!(!app.copies.contains_key(&primary));
  assert_eq!(app.focused, primary);
  app.detach().await;
  Ok(())
}

async fn assert_alternate_screen_drag(
  app: &mut App<'_>,
  child: &str,
  pane_left: u16,
) -> Result<()> {
  // Shift drags select the visible alternate screen, not hidden primary text.
  let visible = app.panes[child].model.vt.line(0).text();
  let expected: String = visible.chars().take(5).collect();
  for (kind, x) in [
    (MouseEventKind::Down(MouseButton::Left), 0),
    (MouseEventKind::Drag(MouseButton::Left), 3),
    (MouseEventKind::Up(MouseButton::Left), 4),
  ] {
    app
      .event(Event::Mouse(MouseEvent {
        kind,
        column: pane_left + x,
        row: 0,
        modifiers: KeyModifiers::SHIFT,
      }))
      .await?;
    if matches!(kind, MouseEventKind::Drag(_)) {
      let frozen = app.copies[child]
        .lines
        .iter()
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
      assert!(!frozen.contains("HIDDEN_PRIMARY"));
      assert_eq!(
        app.copies[child].lines[0].iter().collect::<String>(),
        visible.trim_end()
      );
    }
  }
  assert_eq!(app.copy_buffer.as_deref(), Some(expected.as_str()));
  Ok(())
}
