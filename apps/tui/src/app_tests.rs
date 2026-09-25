use super::*;
use rmux_proto::CommandSpec;
use std::os::unix::fs::PermissionsExt;

struct Daemon {
  directory: PathBuf,
  task: tokio::task::JoinHandle<std::result::Result<(), rmuxd::DaemonError>>,
}

impl Daemon {
  async fn start() -> Result<Self> {
    let directory =
      std::env::temp_dir().join(format!("rtui-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    std::fs::create_dir(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let socket = directory.join("rmux.sock");
    let task = tokio::spawn(rmuxd::run(rmuxd::DaemonConfig {
      socket_path: socket.clone(),
      ..rmuxd::DaemonConfig::default()
    }));
    let daemon = Self { directory, task };
    timeout(Duration::from_secs(5), async {
      while !socket.exists() {
        if daemon.task.is_finished() {
          return Err("test daemon failed to start");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
      Ok(())
    })
    .await??;
    Ok(daemon)
  }

  fn app(&self, read_only: bool) -> App {
    let mut app = App::new(
      self.directory.join("rmux.sock"),
      read_only,
      input::parse_prefix("Ctrl+b").unwrap(),
    );
    app.size = (80, 25);
    app
  }
}

impl Drop for Daemon {
  fn drop(&mut self) {
    self.task.abort();
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

async fn create_shell(app: &App) -> Result<String> {
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

async fn wait_for_text(app: &mut App, id: &str, text: &str) -> Result<()> {
  timeout(Duration::from_secs(5), async {
    loop {
      app.drain().await;
      if app.panes[id].model.vt.text().join("\n").contains(text) {
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
  owner.focus(KeyCode::Left);
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

async fn wait_for_canvas(app: &mut App, columns: u16, rows: u16) -> Result<()> {
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

async fn assert_pane_sizes(app: &mut App) -> Result<()> {
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

async fn wait_for_lease(app: &mut App, lease: LeaseKind, expected: bool) -> Result<()> {
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

async fn assert_lease_handoff(daemon: &Daemon, owner: &mut App, session: &str) -> Result<()> {
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

async fn assert_tmux_shortcuts(app: &mut App) -> Result<()> {
  let focused = app.focused.clone();
  let count = app.panes.len();
  app.select(&focused).await?;
  assert_eq!(app.focused, focused);
  app.command(KeyCode::Char('s')).await?;
  assert!(matches!(app.overlay, Overlay::Sessions(_)));
  assert_eq!(app.panes.len(), count);
  app.overlay = Overlay::None;
  app.command(KeyCode::Char('o')).await?;
  assert_ne!(app.focused, focused);
  app.command(KeyCode::Char('o')).await?;
  assert_eq!(app.focused, focused);
  let owner = app
    .panes
    .values()
    .any(|pane| pane.control.state().leases().layout.owned_by_client);
  app.command(KeyCode::Char('r')).await?;
  assert_eq!(
    app
      .panes
      .values()
      .any(|pane| pane.control.state().leases().layout.owned_by_client),
    owner
  );
  Ok(())
}

async fn assert_copy_mode(app: &mut App, primary: &str) -> Result<()> {
  app.command(KeyCode::Char('[')).await?;
  let snapshot = app.copy_mode.as_ref().unwrap().lines.clone();
  app.event(Event::Paste("IGNORED_PASTE\n".into())).await?;
  app.panes[primary]
    .control
    .input(b"DURING_COPY\n".to_vec())
    .await?;
  wait_for_text(app, primary, "echo:DURING_COPY").await?;
  assert_eq!(app.copy_mode.as_ref().unwrap().lines, snapshot);
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
  assert!(app.copy_mode.is_none());
  app.copy_buffer = Some("BUFFER_PASTE\n".into());
  app.command(KeyCode::Char(']')).await?;
  wait_for_text(app, primary, "echo:BUFFER_PASTE").await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ended_panes_and_confirmed_missing_sessions_wait_for_dismissal() -> Result<()> {
  let daemon = Daemon::start().await?;
  let mut app = daemon.app(false);
  let session = create_shell(&app).await?;
  app.start(Some(session.clone())).await?;
  let primary = app.focused.clone();
  app.split(SplitAxis::Horizontal).await?;
  let child = app.focused.clone();
  app.panes[&child]
    .control
    .input(b"printf 'FINAL_CHILD\\n'; exit 7\n".to_vec())
    .await?;
  timeout(Duration::from_secs(5), async {
    while app.panes[&child].ended.is_none() {
      app.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
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
  .await?;
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
