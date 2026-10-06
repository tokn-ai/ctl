use crate::support::{Result, Screen, TestDaemon, TestProxy, Tui};
use ctmux_client::{
  AttachRequest, AttachmentControl, AttachmentController, AttachmentControllerOptions,
  AttachmentEvent, AttachmentEvents, ClientIdentity,
};
use ctmux_proto::{
  ClientMessage, CommandSpec, DividerResize, PaneGeometry, PaneResizeOutcome, ServerMessage,
  SplitAxis, TerminalSize, ViewInfo,
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
  screen.row(usize::from(ROWS - 1))
}

fn shell(tag: &str) -> CommandSpec {
  CommandSpec {
    program: "/bin/sh".into(),
    arguments: vec!["-c".into(), "PATH=/usr/bin:/bin; export PATH; stty -echo; printf '%s:ready\\n' \"$1\"; while IFS= read -r line; do case \"$line\" in size-*) printf '%s:%s:' \"$1\" \"$line\"; stty size;; *) printf '%s:%s\\n' \"$1\" \"$line\";; esac; done".into(), "ctmux-resize-fixture".into(), tag.into()],
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
  predicate: impl Fn(&ViewInfo) -> bool,
) -> Result<ViewInfo> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let snapshot = view(daemon, session).await?;
    if predicate(&snapshot) {
      return Ok(snapshot);
    }
    if Instant::now() >= deadline {
      return Err(format!("pane resize was not applied: {snapshot:#?}").into());
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

async fn split(
  daemon: &TestDaemon,
  session: &str,
  id: &str,
  axis: SplitAxis,
  tag: &str,
) -> Result<String> {
  let old = match daemon
    .request(ClientMessage::GetView {
      session: session.into(),
    })
    .await?
  {
    ServerMessage::ViewSnapshot { view } => view,
    response => return Err(format!("expected initial view, got {response:?}").into()),
  };
  let ServerMessage::ViewSnapshot { view } = daemon
    .request(ClientMessage::SplitTerminal {
      terminal_id: id.into(),
      axis,
      command: Some(shell(tag)),
      working_directory: None,
      terminal_size: canvas(),
    })
    .await?
  else {
    return Err("expected split view".into());
  };
  Ok(
    view
      .panes
      .iter()
      .find(|pane| {
        !old
          .panes
          .iter()
          .any(|previous| previous.terminal_id == pane.terminal_id)
      })
      .ok_or("new pane missing")?
      .terminal_id
      .clone(),
  )
}

async fn fixture(daemon: &TestDaemon, name: &str) -> Result<(String, String, String)> {
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some(name.into()),
      command: Some(shell("first")),
      working_directory: None,
      terminal_size: canvas(),
    })
    .await?
  else {
    return Err("expected resize session".into());
  };
  let second = split(
    daemon,
    &session.session_id,
    &session.terminal_id,
    SplitAxis::Horizontal,
    "second",
  )
  .await?;
  Ok((session.session_id, session.terminal_id, second))
}

async fn actual_size(
  tui: &mut Tui,
  tag: &str,
  marker: &str,
  geometry: &PaneGeometry,
) -> Result<()> {
  tui.send(format!("size-{marker}\r").as_bytes())?;
  let expected = format!("{tag}:size-{marker}:{} {}", geometry.rows, geometry.columns);
  tui
    .wait_screen("shell reports resized kernel PTY dimensions", |screen| {
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
async fn resize_keys_repeat_move_nearest_dividers_and_atomically_unzoom() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = fixture(&daemon, "nested-resize").await?;
  let third = split(&daemon, &session, &second, SplitAxis::Vertical, "third").await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("nested shells ready", |screen| {
      screen.contains("first:ready")
        && screen.contains("second:ready")
        && screen.contains("third:ready")
    })
    .await?;
  let original = view(&daemon, &session).await?;
  // One prefix, two control arrows, then an Alt arrow uses the same repeat table.
  tui.send(b"\x02\x1b[1;5C\x1b[1;5C\x1b[1;3D")?;
  let horizontal = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&original, &first).columns - 3
  })
  .await?;
  actual_size(&mut tui, "first", "horizontal", pane(&horizontal, &first)).await?;
  assert_eq!(
    pane(&horizontal, &second).rows,
    pane(&original, &second).rows
  );
  // Focus the upper right pane. Down resizes its inner horizontal divider.
  tui.send(b"\x02\x1b[C\x02\x1b[1;5B")?;
  let vertical = wait_view(&daemon, &session, |view| {
    pane(view, &second).rows == pane(&horizontal, &second).rows + 1
  })
  .await?;
  assert_eq!(pane(&vertical, &first), pane(&horizontal, &first));
  assert_eq!(
    pane(&vertical, &third).rows,
    pane(&horizontal, &third).rows - 1
  );
  actual_size(&mut tui, "second", "vertical", pane(&vertical, &second)).await?;
  // The layout owner is still the first attachment; the request targets second.
  tui.send(b"\x02z")?;
  tui
    .wait_screen("second pane is zoomed", |screen| {
      footer(screen).contains("ZOOM") && !screen.contains("third:ready")
    })
    .await?;
  tui.send(b"\x02\x1b[1;5C")?;
  let restored = wait_view(&daemon, &session, |view| {
    view.zoomed_terminal_id.is_none()
      && pane(view, &first).columns == pane(&vertical, &first).columns + 1
  })
  .await?;
  actual_size(&mut tui, "second", "unzoomed", pane(&restored, &second)).await?;
  assert_eq!(pane(&restored, &second).rows, pane(&vertical, &second).rows);
  tui
    .wait_screen("fixed status and siblings return", |screen| {
      !footer(screen).contains("ZOOM")
        && screen.contains("first:ready")
        && screen.contains("third:ready")
    })
    .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pane_resize_preserves_zoom_until_the_client_explicitly_owns_layout() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = fixture(&daemon, "resize-ownership").await?;
  let mut owner = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  let mut observer = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  observer
    .wait_screen("observer lacks resize lease", |screen| {
      footer(screen).contains("shared size")
    })
    .await?;
  owner.send(b"\x02z")?;
  observer
    .wait_screen("observer sees shared zoom", |screen| {
      footer(screen).contains("ZOOM")
    })
    .await?;
  let zoomed = view(&daemon, &session).await?;
  observer.send(b"\x02\x1b[1;3C")?;
  observer
    .wait_screen("resize denial stays visible", |screen| {
      footer(screen).contains("Resize lease required")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?, zoomed);
  owner.send(b"\x02R")?;
  owner
    .wait_screen("owner explicitly releases resize", |screen| {
      footer(screen).contains("shared size")
    })
    .await?;
  observer.send(b"\x02R")?;
  observer
    .wait_screen_for(
      Duration::from_secs(8),
      "observer acquires available resize lease after the denial notice expires",
      |screen| footer(screen).contains("resize owner"),
    )
    .await?;
  observer.send(b"\x02\x1b[1;3C")?;
  let resized = wait_view(&daemon, &session, |view| {
    view.zoomed_terminal_id.is_none()
      && pane(view, &first).columns == pane(&zoomed, &first).columns + 5
  })
  .await?;
  owner
    .wait_screen("first client sees authoritative new divider", |screen| {
      !footer(screen).contains("ZOOM") && screen.contains("second:ready")
    })
    .await?;
  actual_size(&mut owner, "first", "other-owner", pane(&resized, &first)).await?;
  detach(&mut observer).await?;
  drop(observer);
  detach(&mut owner).await?;
  drop(owner);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resized_layout_survives_transport_recovery_and_accepts_more_resize_keys() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, _) = fixture(&daemon, "resize-reconnect").await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("both shells ready", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await?;
  let original = view(&daemon, &session).await?;
  tui.send(b"\x02\x1b[1;3C")?;
  let resized = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&original, &first).columns + 5
  })
  .await?;
  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  tui
    .wait_screen(
      "resized screen stays visible during interruption",
      |screen| footer(screen).starts_with(" reconnecting |"),
    )
    .await?;
  proxy.resume();
  proxy.wait_attachments(before + 2).await?;
  tui
    .wait_screen("resized screen reconnects", |screen| {
      footer(screen).starts_with(" connected |") && screen.contains("second:ready")
    })
    .await?;
  assert_eq!(view(&daemon, &session).await?.layout, resized.layout);
  tui.send(b"\x02\x1b[1;5D")?;
  let recovered = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&resized, &first).columns - 1
  })
  .await?;
  actual_size(&mut tui, "first", "recovered", pane(&recovered, &first)).await?;
  detach(&mut tui).await?;
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

async fn divider_result(
  control: &AttachmentControl,
  events: &mut AttachmentEvents,
  expected: &str,
) -> Result<ViewInfo> {
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      match events
        .recv()
        .await
        .ok_or("divider owner stopped before acknowledgement")?
      {
        AttachmentEvent::Checkpoint { checkpoint, .. } => {
          control.acknowledge_checkpoint(checkpoint.sequence).await?;
        }
        AttachmentEvent::Output { sequence_end, .. } => {
          control.acknowledge_output(sequence_end).await?;
        }
        AttachmentEvent::PtyGeometryChanged {
          observed_sequence, ..
        } => control.acknowledge_geometry(observed_sequence).await?,
        AttachmentEvent::PaneResizeResult {
          request_id,
          outcome,
        } if request_id == expected => {
          return match outcome {
            PaneResizeOutcome::Applied { view } => Ok(*view),
            PaneResizeOutcome::Rejected { message, .. } => Err(message.into()),
          };
        }
        AttachmentEvent::ServerError { message, .. } => return Err(message.into()),
        _ => {}
      }
    }
  })
  .await?
}

type LayoutOwner = (
  AttachmentControl,
  AttachmentEvents,
  tokio::task::JoinHandle<std::result::Result<ctmux_client::AttachExit, ctmux_client::ClientError>>,
);

async fn layout_owner(daemon: &TestDaemon, session: &str) -> Result<LayoutOwner> {
  let (stream, attached) = ctmux_client::begin_attach(
    ctmux_ipc::connect_or_start_daemon(&daemon.socket).await?,
    &ClientIdentity {
      name: "divider-owner".into(),
      version: "test".into(),
    },
    AttachRequest {
      session: session.into(),
      resume_from: None,
      terminal_size: canvas(),
      request_input_lease: false,
      request_layout_lease: true,
      request_command_line: false,
      request_running_command: false,
      presentation_window_bytes: ctmux_client::DEFAULT_PRESENTATION_WINDOW_BYTES,
    },
  )
  .await?;
  assert!(attached.layout_lease.owned_by_client);
  let (controller, control, events) =
    AttachmentController::new(stream, &attached, AttachmentControllerOptions::default())?;
  let runner = tokio::spawn(async move { controller.run().await });
  Ok((control, events, runner))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_client_moves_the_exact_outer_divider_and_the_tui_renders_shared_geometry()
-> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let (session, first, second) = fixture(&daemon, "outer-divider").await?;
  let third = split(&daemon, &session, &first, SplitAxis::Horizontal, "third").await?;
  let original = view(&daemon, &session).await?;
  let (control, mut events, runner) = layout_owner(&daemon, &session).await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen(
      "same-axis nested panes share another client's layout",
      |screen| {
        screen.contains("first:ready")
          && screen.contains("second:ready")
          && screen.contains("third:ready")
          && footer(screen).contains("shared size")
      },
    )
    .await?;
  // The first terminal's nearest horizontal divider is the inner split. The
  // GUI-selected root boundary must instead move the entire left subtree.
  let position = pane(&original, &second).left + 9;
  control
    .resize_divider(
      DividerResize {
        view_id: original.view_id.clone(),
        expected_revision: original.revision,
        split_path: Vec::new(),
        boundary: 0,
        position,
      },
      "outer-drag".into(),
    )
    .await?;
  let changed = divider_result(&control, &mut events, "outer-drag").await?;
  assert_eq!(pane(&changed, &second).left, position + 1);
  assert_eq!(
    pane(&changed, &first).columns,
    pane(&original, &first).columns + 5
  );
  assert_eq!(
    pane(&changed, &third).columns,
    pane(&original, &third).columns + 5
  );
  assert_eq!(
    pane(&changed, &second).columns,
    pane(&original, &second).columns - 10
  );
  let shown = tui
    .wait_screen("outer divider moves in the attached TUI", |screen| {
      screen.rows[..usize::from(ROWS - 1)]
        .iter()
        .all(|row| row.chars().nth(usize::from(position)) == Some('│'))
        && !footer(screen).contains('│')
    })
    .await?;
  assert!(
    shown.contains("first:ready")
      && shown.contains("third:ready")
      && shown.contains("second:ready")
  );
  actual_size(&mut tui, "first", "cross-client", pane(&changed, &first)).await?;
  control.detach().await?;
  assert!(matches!(
    runner.await??.reason,
    ctmux_client::AttachExitReason::Detached
  ));
  tui.send(b"\x02R")?;
  tui
    .wait_screen(
      "TUI explicitly acquires the released resize lease",
      |screen| footer(screen).contains("resize owner"),
    )
    .await?;
  tui.send(b"\x02\x1b[1;5C")?;
  let keyboard = wait_view(&daemon, &session, |view| {
    pane(view, &first).columns == pane(&changed, &first).columns + 1
  })
  .await?;
  assert_eq!(pane(&keyboard, &second), pane(&changed, &second));
  actual_size(
    &mut tui,
    "first",
    "keys-after-drag",
    pane(&keyboard, &first),
  )
  .await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}
