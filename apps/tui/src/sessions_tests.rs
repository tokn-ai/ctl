use super::*;
use crate::{input, test_daemon::TestDaemon};
use ctmux_proto::{ClientMessage, SplitAxis, TerminalSize};

fn app(daemon: &TestDaemon, read_only: bool) -> App<'static> {
  let mut app = App::new(
    daemon.socket.clone(),
    read_only,
    input::parse_prefix("Ctrl+b").unwrap(),
  );
  app.size = (120, 25);
  app
}

fn key(code: KeyCode) -> KeyEvent {
  KeyEvent::new(code, KeyModifiers::NONE)
}

async fn create(daemon: &TestDaemon, name: &str) -> Result<String> {
  daemon
    .create_echo_session(
      name,
      name,
      TerminalSize {
        columns: 120,
        rows: 24,
        ..Default::default()
      },
    )
    .await
}

#[tokio::test]
async fn resume_lists_existing_work_without_attachment_and_empty_resume_creates_once() -> Result<()>
{
  let daemon = TestDaemon::start().await?;
  let existing = create(&daemon, "existing").await?;
  let mut manager = app(&daemon, false);
  manager.start_resume().await?;
  assert!(manager.view.is_none());
  assert!(manager.panes.is_empty());
  assert_eq!(manager.sessions.len(), 1);
  assert!(matches!(&manager.overlay, Overlay::Sessions(Some(id)) if id == &existing));
  assert!(
    manager
      .session_overlay_key(key(KeyCode::Esc))
      .await?
      .unwrap()
  );
  let empty_daemon = TestDaemon::start().await?;
  let mut empty = app(&empty_daemon, false);
  empty.start_resume().await?;
  assert!(empty.view.is_some());
  assert_eq!(empty.sessions.len(), 1);
  empty.detach().await;
  Ok(())
}

#[tokio::test]
async fn blank_creation_uses_daemon_names_while_blank_rename_keeps_the_existing_name() -> Result<()>
{
  let daemon = TestDaemon::start().await?;
  let existing = create(&daemon, "existing").await?;
  let mut manager = app(&daemon, false);
  manager.start_resume().await?;
  manager.session_overlay_key(key(KeyCode::Char('n'))).await?;
  assert!(manager.session_overlay_lines()[0].contains("blank = automatic"));
  manager.session_overlay_key(key(KeyCode::Enter)).await?;
  let generated = manager.view.as_ref().unwrap().clone();
  assert_ne!(generated.session_id, existing);
  assert_ne!(generated.session_name.trim(), "");
  manager.open_sessions();
  manager.session_overlay_key(key(KeyCode::Char('r'))).await?;
  manager
    .session_overlay_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
    .await?;
  manager.session_overlay_key(key(KeyCode::Enter)).await?;
  assert!(matches!(manager.overlay, Overlay::SessionName(_)));
  assert!(manager.message.contains("Session name cannot be empty"));
  assert_eq!(
    manager.view.as_ref().unwrap().session_name,
    generated.session_name
  );
  manager.session_overlay_key(key(KeyCode::Esc)).await?;
  manager.session_overlay_key(key(KeyCode::Esc)).await?;
  for command in ["new-session -s ''", "new-session -s '   '"] {
    manager
      .execute_command(crate::prompt::parse(command)?)
      .await?;
    let current = manager.view.as_ref().unwrap();
    assert_ne!(current.session_name.trim(), "");
    assert_ne!(current.session_name, generated.session_name);
    assert_ne!(current.session_id, generated.session_id);
  }
  assert_eq!(manager.sessions.len(), 4);
  manager.detach().await;
  Ok(())
}

#[tokio::test]
async fn manager_rejects_read_only_mutations_and_pins_disappeared_targets() -> Result<()> {
  let daemon = TestDaemon::start().await?;
  let existing = create(&daemon, "reused-name").await?;
  create(&daemon, "keeper").await?;
  let mut observer = app(&daemon, true);
  observer.start_resume().await?;
  for code in ['n', 'r', 'x'] {
    observer
      .session_overlay_key(key(KeyCode::Char(code)))
      .await?;
    assert!(matches!(&observer.overlay, Overlay::Sessions(Some(id)) if id == &existing));
    assert!(observer.message.contains("read only"));
  }
  let mut manager = app(&daemon, false);
  manager.start_resume().await?;
  manager.session_overlay_key(key(KeyCode::Char('x'))).await?;
  daemon
    .request(ClientMessage::KillSession {
      session: existing.clone(),
    })
    .await?;
  tokio::time::timeout(Duration::from_secs(3), async {
    loop {
      manager.list().await?;
      if manager
        .sessions
        .iter()
        .all(|session| session.session_id != existing)
      {
        return Ok::<_, crate::Error>(());
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await??;
  let replacement = create(&daemon, "reused-name").await?;
  manager.list().await?;
  let error = manager
    .session_overlay_key(key(KeyCode::Char('y')))
    .await
    .unwrap_err();
  assert!(super::super::session_not_found(&error));
  assert!(matches!(&manager.overlay, Overlay::Sessions(Some(id)) if id == &existing));
  assert!(
    manager
      .sessions
      .iter()
      .any(|session| session.session_id == replacement)
  );
  manager.session_overlay_key(key(KeyCode::Enter)).await?;
  assert!(manager.view.is_none());
  assert!(manager.ended.is_none());
  Ok(())
}

#[tokio::test]
async fn rename_preserves_identity_layout_and_names_in_other_attached_clients() -> Result<()> {
  let daemon = TestDaemon::start().await?;
  let session = create(&daemon, "before").await?;
  let mut owner = app(&daemon, false);
  owner.start(Some(session.clone())).await?;
  owner.split(SplitAxis::Horizontal).await?;
  let original = owner.view.clone().unwrap();
  let mut viewer = app(&daemon, true);
  viewer.start(Some(session.clone())).await?;
  let target = owner.resolve_session_target(None).await?;
  owner
    .maintenance
    .refresh(None, daemon.socket.clone(), Some(session.clone()));
  owner.rename_session(&target, "after".into()).await?;
  assert!(owner.maintenance.poll().await.snapshot.is_none());
  let renamed = owner.view.as_ref().unwrap();
  assert_eq!(renamed.session_name, "after");
  assert_eq!(renamed.session_id, original.session_id);
  assert_eq!(renamed.revision, original.revision + 1);
  assert_eq!(renamed.layout, original.layout);
  tokio::time::timeout(Duration::from_secs(3), async {
    while viewer.view.as_ref().unwrap().session_name != "after" {
      viewer.drain().await;
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await?;
  // An exact ID wins over another session with that ID as its display name.
  create(&daemon, &session).await?;
  assert_eq!(
    owner.resolve_session_target(Some(&session)).await?.id,
    session
  );
  owner.detach().await;
  viewer.detach().await;
  Ok(())
}

#[tokio::test]
async fn cancelling_termination_preserves_panes_and_confirming_active_returns_empty_manager()
-> Result<()> {
  let daemon = TestDaemon::start().await?;
  let session = create(&daemon, "all-panes").await?;
  let mut owner = app(&daemon, false);
  owner.start(Some(session.clone())).await?;
  owner.split(SplitAxis::Horizontal).await?;
  owner.open_sessions();
  owner.session_overlay_key(key(KeyCode::Char('x'))).await?;
  assert!(
    owner
      .session_overlay_lines()
      .iter()
      .any(|line| line.contains("ALL panes"))
  );
  owner.session_overlay_key(key(KeyCode::Esc)).await?;
  assert_eq!(owner.view.as_ref().unwrap().terminals.len(), 2);
  owner.session_overlay_key(key(KeyCode::Char('x'))).await?;
  // Archive failure must not prevent returning from a successfully terminated session.
  let blocked = daemon.directory.join("archive-file");
  std::fs::write(&blocked, "not a directory")?;
  owner.archive_directory = Some(blocked);
  owner.session_overlay_key(key(KeyCode::Char('y'))).await?;
  assert!(owner.view.is_none());
  assert!(owner.panes.is_empty());
  assert_eq!(owner.sessions, []);
  assert!(matches!(owner.overlay, Overlay::Sessions(None)));
  assert!(owner.message.contains("could not save archive"));
  assert!(owner.session_overlay_key(key(KeyCode::Esc)).await?.unwrap());
  Ok(())
}
