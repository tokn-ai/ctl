use super::*;
use crate::actions::Action;
use crate::keys::KeyState;
use crate::test_daemon::TestDaemon as Daemon;
use ctmux_proto::CommandSpec;

struct RelayTransport {
  socket: PathBuf,
  connections: std::sync::atomic::AtomicUsize,
  fail_next: std::sync::atomic::AtomicBool,
}

impl crate::Transport for RelayTransport {
  fn connect(&self) -> crate::ConnectFuture<'_> {
    Box::pin(async move {
      if self
        .fail_next
        .swap(false, std::sync::atomic::Ordering::AcqRel)
      {
        return Err(io::Error::from(io::ErrorKind::BrokenPipe).into());
      }
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
    fail_next: std::sync::atomic::AtomicBool::new(false),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn successful_refresh_restores_control_status_without_clearing_action_notices() -> Result<()>
{
  let daemon = Daemon::start().await?;
  let transport = RelayTransport {
    socket: daemon.directory.join("ctmux.sock"),
    connections: std::sync::atomic::AtomicUsize::new(0),
    fail_next: std::sync::atomic::AtomicBool::new(false),
  };
  let mut app = daemon.app(false);
  app.transport = Some(&transport);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  transport
    .fail_next
    .store(true, std::sync::atomic::Ordering::Release);
  app.schedule_refresh();
  timeout(Duration::from_secs(2), async {
    while app.notice_kind != NoticeKind::Connection {
      app.poll_maintenance().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  assert!(app.status().contains("Disconnected:"));
  assert!(!app.status().contains("pane 1"));
  let previous_deadline = app.message_until;
  app.schedule_refresh();
  timeout(Duration::from_secs(2), async {
    while app.notice_kind == NoticeKind::Connection {
      app.poll_maintenance().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  assert!(
    Instant::now() < previous_deadline,
    "recovery must clear the notice before its six-second expiry"
  );
  assert!(app.status().contains("pane 1"));
  assert!(!app.status().contains("Disconnected:"));

  app.notice("Copied selection".into());
  let action_deadline = app.message_until;
  app.sessions.clear();
  app.schedule_refresh();
  timeout(Duration::from_secs(2), async {
    while app.sessions.is_empty() {
      app.poll_maintenance().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  assert!(app.status().contains("Copied selection"));
  assert_eq!(app.message_until, action_deadline);
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_zoom_preserves_hidden_attachments_and_frozen_selections() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut owner = daemon.app(false);
  let session = create_shell(&owner).await?;
  owner.start(Some(session.clone())).await?;
  let first = owner.focused.clone();
  let view = daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", owner.canvas_size())
    .await?;
  let second = view
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != first)
    .unwrap()
    .terminal_id
    .clone();
  owner.refresh().await?;
  wait_for_text(&mut owner, &second, "second:ready").await?;
  owner.panes[&first]
    .control
    .input(b"BEFORE_ZOOM\n".to_vec())
    .await?;
  wait_for_text(&mut owner, &first, "echo:BEFORE_ZOOM").await?;
  owner.execute(Action::History { page_back: false }).await?;
  owner
    .key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
    .await?;
  let frozen = owner.copies[&first].lines.clone();
  let cursor = owner.copies[&first].cursor;
  assert!(owner.copies[&first].selected(cursor));
  let tokens: BTreeMap<_, _> = owner
    .panes
    .iter()
    .map(|(id, pane)| (id.clone(), pane.token.clone()))
    .collect();

  owner.execute(Action::NextPane).await?;
  assert_eq!(owner.focused, second);
  owner.execute(Action::ToggleZoom).await?;
  assert_eq!(
    owner.view.as_ref().unwrap().zoomed_terminal_id.as_ref(),
    Some(&second)
  );
  assert_eq!(owner.panes.len(), 2);
  owner.panes[&first]
    .control
    .input(b"WHILE_HIDDEN\n".to_vec())
    .await?;
  wait_for_text(&mut owner, &first, "echo:WHILE_HIDDEN").await?;
  assert_eq!(owner.copies[&first].lines, frozen);
  assert!(owner.copies[&first].selected(cursor));
  assert!(owner.status().contains("ZOOM"));

  let mut observer = daemon.app(true);
  observer.start(Some(session)).await?;
  assert_eq!(observer.focused, second);
  let error = observer.execute(Action::ToggleZoom).await.unwrap_err();
  assert!(error.to_string().contains("Resize lease required"));
  assert_eq!(
    observer.view.as_ref().unwrap().zoomed_terminal_id.as_ref(),
    Some(&second)
  );

  // Normal focus navigation restores the shared split before changing focus.
  owner.execute(Action::NextPane).await?;
  assert_eq!(owner.focused, first);
  assert!(owner.view.as_ref().unwrap().zoomed_terminal_id.is_none());
  assert_eq!(owner.copies[&first].lines, frozen);
  assert!(owner.copies[&first].selected(cursor));
  for (id, token) in tokens {
    assert_eq!(owner.panes[&id].token, token);
    assert!(
      owner.panes[&id]
        .control
        .state()
        .leases()
        .input
        .owned_by_client
    );
  }
  observer.detach().await;
  owner.detach().await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prompt_errors_and_ended_session_cancel_preserve_frozen_copy_state() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let pane = app.focused.clone();
  app.panes[&pane]
    .control
    .input(b"COPY_MARK\n".to_vec())
    .await?;
  wait_for_text(&mut app, &pane, "echo:COPY_MARK").await?;
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&pane].lines.clone();

  for (command, notice) in [
    ("switch-client -t missing-session", "Session not found:"),
    ("unknown-command", "Unknown command:"),
  ] {
    app.execute(Action::CommandPrompt).await?;
    app.event(Event::Paste(command.into())).await?;
    assert!(
      !app
        .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await?
    );
    assert!(!app.prompt.is_active());
    assert!(app.ended.is_none());
    assert_eq!(app.focused, pane);
    assert_eq!(app.selected_id, session);
    assert_eq!(app.copies[&pane].lines, frozen);
    assert!(app.message.starts_with(notice), "{}", app.message);
    assert!(app.status().contains(notice));
    assert!(app.status().contains("COPY"));
  }

  app.ended = Some("Session ended — press any key to exit".into());
  app.key(app.prefix.key).await?;
  assert!(
    !app
      .key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE))
      .await?
  );
  assert!(app.prompt.is_active());
  assert!(
    !app
      .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
      .await?
  );
  assert!(!app.prompt.is_active());
  assert_eq!(app.copies[&pane].lines, frozen);
  assert!(app.panes.contains_key(&pane));
  app.detach().await;
  Ok(())
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
  // Deliberately leave final presentation events buffered until their
  // controller has stopped. Closed acknowledgements must not discard the
  // remaining output or replace the real exit code with a missing-pane notice.
  timeout(
    Duration::from_secs(5),
    app
      .panes
      .get_mut(&child)
      .unwrap()
      .wait_for_controller_exit(),
  )
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
      .contains("FINAL_CHILD"),
    "ended={:?}",
    app.panes[&child].ended
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zoom_ack_preserves_ended_sibling_copy_state_until_archival() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let child = split_exit_shell(&mut app).await?;
  app.execute(Action::History { page_back: false }).await?;
  app
    .key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE))
    .await?;
  let frozen = app.copies[&child].lines.clone();
  let cursor = app.copies[&child].cursor;
  let token = app.panes[&child].token.clone();
  assert!(app.copies[&child].selected(cursor));
  app.focused.clone_from(&primary);
  app.panes[&child]
    .control
    .input(b"finish\n".to_vec())
    .await?;
  timeout(
    Duration::from_secs(5),
    app
      .panes
      .get_mut(&child)
      .unwrap()
      .wait_for_controller_exit(),
  )
  .await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&child].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  assert_eq!(app.focused, primary);
  assert!(
    app.panes[&child]
      .model
      .copy_lines()
      .join("\n")
      .contains("FINAL_CHILD")
  );

  // The canonical acknowledgement omits the ended sibling. Zoom must keep
  // that client's retained model and selection until normal dismissal.
  for zoomed in [true, false] {
    app.execute(Action::ToggleZoom).await?;
    assert_eq!(
      app.view.as_ref().unwrap().zoomed_terminal_id.is_some(),
      zoomed
    );
    assert_eq!(app.panes.len(), 2);
    assert_eq!(app.panes[&child].token, token);
    assert_eq!(app.copies[&child].lines, frozen);
    assert!(app.copies[&child].selected(cursor));
    assert!(
      app.panes[&child]
        .model
        .copy_lines()
        .join("\n")
        .contains("FINAL_CHILD")
    );
  }
  app.focused.clone_from(&child);
  // Copy mode remains usable; leaving it precedes the normal ended-pane key.
  app
    .key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
    .await?;
  assert!(app.panes.contains_key(&child));
  assert!(
    !app
      .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
      .await?
  );
  assert_eq!(app.panes.len(), 1);
  assert_eq!(app.focused, primary);
  assert!(app.local_archives()?.iter().any(|archive| {
    archive
      .terminals
      .iter()
      .any(|pane| pane.terminal_id == child && pane.lines.join("\n").contains("FINAL_CHILD"))
  }));
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foreign_view_revision_cannot_hide_a_current_view_zoom_update() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let primary = app.focused.clone();
  let child = split_exit_shell(&mut app).await?;
  let mut current = app.view.as_ref().unwrap().clone();
  current.revision += 1;
  current.zoomed_terminal_id = Some(primary.clone());
  let mut foreign = current.clone();
  foreign.view_id = format!("foreign-{}", foreign.view_id);
  foreign.session_id = "foreign-session".into();
  foreign.revision += 100;
  foreign.zoomed_terminal_id = Some(child.clone());
  // A promoted terminal can begin receiving another view's snapshots. Its
  // larger revision must be rejected before selecting the newest candidate.
  app.panes.get_mut(&primary).unwrap().view_update = Some(current.clone());
  app.panes.get_mut(&child).unwrap().view_update = Some(foreign);
  app.drain().await;
  assert_eq!(app.view.as_ref(), Some(&current));
  assert_eq!(app.focused, primary);
  assert_eq!(app.panes.len(), 2);
  app.detach().await;
  Ok(())
}

async fn split_migration_echo(daemon: &Daemon, app: &mut App<'_>) -> Result<String> {
  let first = app.focused.clone();
  let view = daemon
    .split_echo(&first, SplitAxis::Horizontal, "moving", app.canvas_size())
    .await?;
  let second = view
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != first)
    .ok_or("missing split pane")?
    .terminal_id
    .clone();
  app.refresh().await?;
  wait_for_text(app, &second, "moving:ready").await?;
  Ok(second)
}

async fn external_break(app: &mut App<'_>, terminal: &str) -> Result<(ViewInfo, ViewInfo)> {
  let view = app.view.as_ref().ok_or("missing source view")?;
  let target = ctmux_proto::PaneTarget {
    session_id: view.session_id.clone(),
    view_id: view.view_id.clone(),
    expected_revision: view.revision,
    terminal_id: terminal.into(),
  };
  let owner = app
    .panes
    .iter()
    .find(|(_, pane)| pane.control.state().leases().layout.owned_by_client)
    .ok_or("missing layout owner")?
    .0
    .clone();
  app
    .panes
    .get_mut(&owner)
    .unwrap()
    .break_pane(target, Some("promoted".into()), "migration-test".into())
    .await?;
  timeout(Duration::from_secs(5), async {
    loop {
      let pane = app.panes.get_mut(&owner).unwrap();
      pane.drain().await?;
      if let Some(outcome) = pane.move_result.take() {
        return match outcome {
          ctmux_proto::PaneMoveOutcome::Promoted { view, source_view } => Ok((*view, *source_view)),
          outcome => Err(format!("unexpected pane move: {outcome:?}").into()),
        };
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_migration_removes_live_pane_without_archiving_or_remounting_siblings()
-> Result<()> {
  let daemon = Daemon::start().await?;
  let mut owner = daemon.app(false);
  let session = create_shell(&owner).await?;
  owner.start(Some(session.clone())).await?;
  let first = owner.focused.clone();
  let moving = split_migration_echo(&daemon, &mut owner).await?;
  let mut observer = daemon.app(true);
  observer.start(Some(session.clone())).await?;
  observer
    .execute(Action::History { page_back: false })
    .await?;
  let frozen = observer.copies[&first].lines.clone();
  let token = observer.panes[&first].token.clone();
  observer.focused.clone_from(&moving);
  observer
    .execute(Action::History { page_back: false })
    .await?;
  let (promoted, source) = external_break(&mut owner, &moving).await?;
  timeout(Duration::from_secs(5), async {
    while observer.panes.contains_key(&moving) {
      observer.drain().await;
      observer.schedule_refresh();
      observer.poll_maintenance().await;
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Result::<()>::Ok(())
  })
  .await??;
  assert_eq!(observer.selected_id, session);
  assert_eq!(observer.view.as_ref(), Some(&source));
  assert_eq!(observer.focused, first);
  assert_eq!(observer.panes[&first].token, token);
  assert!(observer.panes[&first].connected);
  assert_eq!(observer.copies[&first].lines, frozen);
  assert!(!observer.copies.contains_key(&moving));
  assert!(observer.migrated_panes.is_empty());
  assert!(observer.archived_panes.is_empty());
  assert!(observer.local_archives()?.is_empty());
  assert_ne!(promoted.session_id, session);
  assert_eq!(promoted.terminals[0].terminal_id, moving);
  observer.detach().await;
  owner.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_requires_foreign_membership_and_matching_source_omission() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let moving = split_migration_echo(&daemon, &mut app).await?;
  let current = app.view.as_ref().unwrap().clone();
  let mut foreign = current.clone();
  foreign.session_id = "another-session".into();
  foreign.view_id = "another-view".into();
  let mut invalid = foreign.clone();
  invalid
    .terminals
    .retain(|terminal| terminal.terminal_id != moving);
  let mut same_session = foreign.clone();
  same_session.session_id.clone_from(&current.session_id);
  let mut same_view = foreign.clone();
  same_view.view_id.clone_from(&current.view_id);
  app.observe_migrations(vec![
    (moving.clone(), invalid),
    (moving.clone(), same_session),
    (moving.clone(), same_view),
  ]);
  assert!(app.migrated_panes.is_empty());
  app.observe_migrations(vec![(moving.clone(), foreign.clone())]);
  assert!(app.migrated_panes.contains(&moving));
  app.reconcile_migrations(&current).await?;
  assert!(app.panes.contains_key(&moving));
  foreign
    .terminals
    .retain(|terminal| terminal.terminal_id != moving);
  app.reconcile_migrations(&foreign).await?;
  assert!(app.panes.contains_key(&moving));
  let mut source = current.clone();
  source
    .terminals
    .retain(|terminal| terminal.terminal_id != moving);
  source.panes.retain(|pane| pane.terminal_id != moving);
  source.layout = ctmux_proto::ViewLayout::Terminal {
    terminal_id: app.focused.clone(),
  };
  source.revision += 1;
  app.reconcile_migrations(&source).await?;
  app.adopt_view(source.clone()).await?;
  assert!(!app.panes.contains_key(&moving));
  assert_eq!(app.view.as_ref(), Some(&source));
  assert!(app.migrated_panes.is_empty());
  assert!(app.local_archives()?.is_empty());
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_proof_survives_pending_break_until_acknowledgement_or_cancellation() -> Result<()>
{
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let moving = split_migration_echo(&daemon, &mut app).await?;
  let current = app.view.as_ref().unwrap().clone();
  app.focused.clone_from(&moving);
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&moving].lines.clone();
  let token = app.panes[&moving].token.clone();
  app.break_pane(Some("protected".into()), false).await?;
  assert!(
    app
      .pane_move
      .as_ref()
      .is_some_and(|pending| pending.promoting(&moving))
  );
  let source = timeout(Duration::from_secs(5), async {
    loop {
      let ServerMessage::ViewSnapshot { view } = app
        .request(ClientMessage::GetView {
          session: session.clone(),
        })
        .await?
      else {
        return Err("expected source view".into());
      };
      if view
        .terminals
        .iter()
        .all(|terminal| terminal.terminal_id != moving)
      {
        return Result::<ViewInfo>::Ok(view);
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await??;
  let ServerMessage::ViewSnapshot { view: promoted } = app
    .request(ClientMessage::GetView {
      session: "protected".into(),
    })
    .await?
  else {
    return Err("expected promoted view".into());
  };
  app.observe_migrations(vec![(moving.clone(), promoted)]);
  app.reconcile_migrations(&source).await?;
  assert!(app.migrated_panes.contains(&moving));
  assert_eq!(app.view.as_ref(), Some(&current));
  assert_eq!(app.panes[&moving].token, token);
  assert_eq!(app.copies[&moving].lines, frozen);
  app.cancel_pane_move();
  app.adopt_view(source.clone()).await?;
  assert_eq!(app.view.as_ref(), Some(&source));
  assert!(!app.panes.contains_key(&moving));
  assert!(!app.copies.contains_key(&moving));
  assert!(app.local_archives()?.is_empty());
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migration_removes_live_pane_while_retaining_an_exited_siblings_final_output() -> Result<()>
{
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let first = app.focused.clone();
  let moving = split_migration_echo(&daemon, &mut app).await?;
  let exited = split_exit_shell(&mut app).await?;
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&exited].lines.clone();
  app.panes[&exited]
    .control
    .input(b"finish\n".to_vec())
    .await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&exited].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await?;
  let token = app.panes[&exited].token.clone();
  // A remote controller uses the current authoritative revision, while this
  // observer retains the exited sibling's final presentation and selection.
  let ServerMessage::ViewSnapshot { view } = app
    .request(ClientMessage::GetView {
      session: app.selected_id.clone(),
    })
    .await?
  else {
    return Err("expected source view".into());
  };
  app.view.as_mut().unwrap().revision = view.revision;
  let (promoted, source) = external_break(&mut app, &moving).await?;
  app.observe_migrations(vec![(moving.clone(), promoted)]);
  app.adopt_view(source).await?;
  assert!(!app.panes.contains_key(&moving));
  assert!(app.panes.contains_key(&first));
  assert_eq!(app.panes[&exited].token, token);
  assert_eq!(app.copies[&exited].lines, frozen);
  assert!(
    app.panes[&exited]
      .model
      .copy_lines()
      .join("\n")
      .contains("FINAL_CHILD")
  );
  assert!(
    app
      .view
      .as_ref()
      .unwrap()
      .terminals
      .iter()
      .all(|terminal| terminal.terminal_id != moving)
  );
  assert!(app.migrated_panes.is_empty());
  assert!(app.local_archives()?.is_empty());
  app.detach().await;
  Ok(())
}

async fn pending_break_snapshots(
  app: &mut App<'_>,
  moving: &str,
  detached: bool,
) -> Result<(String, ViewInfo, ViewInfo)> {
  let owner = app
    .panes
    .iter()
    .find(|(_, pane)| pane.control.state().leases().layout.owned_by_client)
    .ok_or("missing layout owner")?
    .0
    .clone();
  moving.clone_into(&mut app.focused);
  app
    .break_pane(Some("snapshot-promotion".into()), detached)
    .await?;
  let (promoted, source) = timeout(Duration::from_secs(5), async {
    loop {
      let pane = app.panes.get_mut(&owner).unwrap();
      pane.drain().await?;
      if let Some(outcome) = pane.move_result.take() {
        return match outcome {
          ctmux_proto::PaneMoveOutcome::Promoted { view, source_view } => {
            Result::<(ViewInfo, ViewInfo)>::Ok((*view, *source_view))
          }
          outcome => Err(format!("unexpected pending break: {outcome:?}").into()),
        };
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await??;
  // The exact attachment reply has arrived, but the App has not consumed it.
  // Keep it outside the regular drain while a later snapshot is observed.
  assert!(app.pane_move.is_some());
  Ok((owner, promoted, source))
}

async fn wait_for_view_columns(app: &App<'_>, session: &str, columns: u16) -> Result<ViewInfo> {
  timeout(Duration::from_secs(5), async {
    loop {
      let ServerMessage::ViewSnapshot { view } = app
        .request(ClientMessage::GetView {
          session: session.into(),
        })
        .await?
      else {
        return Err("expected resized view".into());
      };
      if view.canvas_size.columns == columns {
        return Result::<ViewInfo>::Ok(view);
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detached_break_old_acknowledgement_keeps_newer_source_geometry_and_removes_moved_pane()
-> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let first = app.focused.clone();
  let moving = split_migration_echo(&daemon, &mut app).await?;
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&first].lines.clone();
  let token = app.panes[&first].token.clone();
  let (owner, promoted, source) = pending_break_snapshots(&mut app, &moving, true).await?;
  app.panes[&owner]
    .control
    .resize(TerminalSize {
      columns: 99,
      rows: 29,
      pixel_width: 0,
      pixel_height: 0,
    })
    .await?;
  let newer = wait_for_view_columns(&app, &session, 99).await?;
  assert!(newer.revision > source.revision);
  app.adopt_view(newer.clone()).await?;
  assert!(app.panes.contains_key(&moving));
  assert_eq!(app.view.as_ref().unwrap().revision, newer.revision);
  app.panes.get_mut(&owner).unwrap().move_result = Some(ctmux_proto::PaneMoveOutcome::Promoted {
    view: Box::new(promoted),
    source_view: Box::new(source),
  });
  app.poll_pane_move().await?;
  assert!(app.pane_move.is_none());
  assert_eq!(app.selected_id, session);
  assert_eq!(app.view.as_ref(), Some(&newer));
  assert_eq!(app.focused, first);
  assert!(!app.panes.contains_key(&moving));
  assert_eq!(app.panes[&first].token, token);
  assert_eq!(app.copies[&first].lines, frozen);
  assert!(app.migrated_panes.is_empty());
  assert!(app.local_archives()?.is_empty());
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn following_break_old_acknowledgement_keeps_newer_promoted_geometry_and_copy_state()
-> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let moving = split_migration_echo(&daemon, &mut app).await?;
  app.focused.clone_from(&moving);
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&moving].lines.clone();
  let token = app.panes[&moving].token.clone();
  let (owner, promoted, source) = pending_break_snapshots(&mut app, &moving, false).await?;
  // Another attachment can resize the newly created root before this App
  // sees its delayed promotion reply. It owns only the available new lease.
  let mut observer = daemon.app(false);
  observer.size = (101, 31);
  observer.start(Some(promoted.session_id.clone())).await?;
  let newer = observer.view.as_ref().unwrap().clone();
  assert!(newer.revision > promoted.revision);
  assert_eq!(newer.canvas_size.columns, 101);
  app.adopt_view(newer.clone()).await?;
  assert_eq!(app.view.as_ref().unwrap().session_id, session);
  app.panes.get_mut(&owner).unwrap().move_result = Some(ctmux_proto::PaneMoveOutcome::Promoted {
    view: Box::new(promoted),
    source_view: Box::new(source),
  });
  app.poll_pane_move().await?;
  assert!(app.pane_move.is_none());
  assert_eq!(app.view.as_ref(), Some(&newer));
  assert_eq!(app.focused, moving);
  assert_eq!(app.panes.len(), 1);
  assert_eq!(app.panes[&moving].token, token);
  assert_eq!(app.copies[&moving].lines, frozen);
  assert!(
    app.panes[&moving]
      .control
      .state()
      .leases()
      .input
      .owned_by_client
  );
  assert!(app.local_archives()?.is_empty());
  observer.detach().await;
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn detached_break_preserves_an_exited_siblings_final_output_and_frozen_selection()
-> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let moving = split_migration_echo(&daemon, &mut app).await?;
  let exited = split_exit_shell(&mut app).await?;
  app.execute(Action::History { page_back: false }).await?;
  let frozen = app.copies[&exited].lines.clone();
  app.panes[&exited]
    .control
    .input(b"finish\n".to_vec())
    .await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&exited].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  })
  .await?;
  let token = app.panes[&exited].token.clone();
  let ServerMessage::ViewSnapshot { view } = app
    .request(ClientMessage::GetView {
      session: session.clone(),
    })
    .await?
  else {
    return Err("expected source view after exit".into());
  };
  app.adopt_view(view).await?;
  let (owner, promoted, source) = pending_break_snapshots(&mut app, &moving, true).await?;
  app.panes.get_mut(&owner).unwrap().move_result = Some(ctmux_proto::PaneMoveOutcome::Promoted {
    view: Box::new(promoted),
    source_view: Box::new(source),
  });
  app.poll_pane_move().await?;
  assert_eq!(app.selected_id, session);
  assert!(!app.panes.contains_key(&moving));
  assert_eq!(app.panes[&exited].token, token);
  assert_eq!(app.copies[&exited].lines, frozen);
  assert!(
    app.panes[&exited]
      .model
      .copy_lines()
      .join("\n")
      .contains("FINAL_CHILD")
  );
  assert!(app.local_archives()?.is_empty());
  app.focused.clone_from(&exited);
  app
    .key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
    .await?;
  app
    .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    .await?;
  assert!(!app.panes.contains_key(&exited));
  assert!(app.local_archives()?.iter().any(|archive| {
    archive
      .terminals
      .iter()
      .any(|pane| pane.terminal_id == exited && pane.lines.join("\n").contains("FINAL_CHILD"))
  }));
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
    (KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::ALT),
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
  // Selecting the current root is a no-op for its live attachments and copies.
  assert!(app.copies.contains_key(&child));
  assert!(!app.copies.contains_key(&primary));
  app.detach().await;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_labels_swallow_cancelled_mouse_gestures_and_allow_later_application_clicks()
-> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  app.size = (80, 10);
  let session = create_shell(&app).await?;
  app.start(Some(session)).await?;
  let child = split_program(&mut app, "stty -echo -icanon; printf '\\033[?1002;1006hMOUSE_READY\\n'; IFS= read -r discarded; if test -z \"$discarded\"; then printf 'DISMISSAL_CLEAN\\n'; else printf 'DISMISSAL_INPUT\\n'; fi; dd bs=1 count=9 2>/dev/null | od -An -tx1; dd bs=1 count=10 2>/dev/null | od -An -tx1; dd bs=1 count=9 2>/dev/null | od -An -tx1; printf 'REPORTS_DONE\\n'; sleep 30").await?;
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
  app.display_panes().await?;
  for kind in [
    MouseEventKind::Down(MouseButton::Left),
    MouseEventKind::Drag(MouseButton::Left),
    MouseEventKind::Up(MouseButton::Left),
  ] {
    app.event(mouse(kind, rect.left + 2, rect.top + 1)).await?;
  }
  assert!(app.pane_labels.is_none());
  assert!(app.mouse_capture.is_none());
  // This newline follows any mouse reports in the same ordered attachment.
  // The child confirms the whole cancelled gesture delivered no bytes.
  app.panes[&child].control.input(b"\n".to_vec()).await?;
  wait_for_text(&mut app, &child, "DISMISSAL_").await?;
  assert!(
    app.panes[&child]
      .model
      .copy_lines()
      .join("\n")
      .contains("DISMISSAL_CLEAN")
  );
  for (kind, column) in [
    (MouseEventKind::Down(MouseButton::Left), 5),
    (MouseEventKind::Drag(MouseButton::Left), 6),
    (MouseEventKind::Up(MouseButton::Left), 6),
  ] {
    app
      .event(mouse(kind, rect.left + column, rect.top + 2))
      .await?;
  }
  wait_for_text(&mut app, &child, "REPORTS_DONE").await?;
  let received = app.panes[&child]
    .model
    .copy_lines()
    .join("\n")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ");
  for report in [
    "1b 5b 3c 30 3b 36 3b 33 4d",
    "1b 5b 3c 33 32 3b 37 3b 33 4d",
    "1b 5b 3c 30 3b 37 3b 33 6d",
  ] {
    assert!(received.contains(report), "{received}");
  }
  assert_eq!(app.focused, child);
  assert!(app.mouse_capture.is_none());
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
