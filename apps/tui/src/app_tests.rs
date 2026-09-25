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
