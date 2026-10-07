use crate::support::{Result, Screen, TestDaemon, TestProxy, Tui};
use ctmux_proto::{ClientMessage, ServerMessage, SplitAxis, TerminalSize};
use std::time::Duration;
use tokio::time::timeout;

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

async fn wait_split(tui: &mut Tui) -> Result<Screen> {
  tui
    .wait_screen("connected split with its fixed status row", |screen| {
      screen.contains("first:ready")
        && screen.contains("second:ready")
        && screen.rows[..usize::from(ROWS - 1)]
          .iter()
          .all(|row| row.contains('│'))
        && footer(screen).starts_with(" connected |")
        && !footer(screen).contains('│')
    })
    .await
}

async fn help(tui: &mut Tui) -> Result<()> {
  tui.send(b"\x02?")?;
  tui
    .wait_screen("prefix help remains usable", |screen| {
      screen.contains("Commands after Ctrl+b")
    })
    .await?;
  tui.send(b"\x1b")?;
  tui
    .wait_screen("help closes back to the terminal", |screen| {
      !screen.contains("Commands after Ctrl+b")
    })
    .await?;
  Ok(())
}

async fn split_session(daemon: &TestDaemon, name: &str) -> Result<String> {
  let session = daemon.create_echo_session(name, "first", canvas()).await?;
  let ServerMessage::ViewSnapshot { view } = daemon
    .request(ClientMessage::GetView {
      session: session.clone(),
    })
    .await?
  else {
    return Err("expected reconnect fixture view".into());
  };
  let first = &view.terminals[0].terminal_id;
  let view = daemon
    .split_echo(first, SplitAxis::Horizontal, "second", canvas())
    .await?;
  assert_eq!(view.terminals.len(), 2);
  Ok(session)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_transport_reconnects_preserve_panes_controls_and_actual_input() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = split_session(&daemon, "repeated-reconnect").await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  wait_split(&mut tui).await?;
  let process_id = tui.process_id();

  for cycle in 0..4 {
    let before = proxy.attachments();
    let stalled = proxy.stalled();
    proxy.interrupt();
    // The barrier proves a replacement operation actually reached the blocked
    // transport; a retained connected frame cannot stand in for a reconnect.
    proxy.wait_stalled(stalled).await?;
    proxy.resume();
    proxy.wait_attachments(before + 2).await?;
    wait_split(&mut tui).await?;
    assert_eq!(tui.process_id(), process_id);
    help(&mut tui).await?;

    for (navigation, tag, pane) in [
      (b"\x02\x1b[D".as_slice(), "first", 1),
      (b"\x02\x1b[C".as_slice(), "second", 2),
    ] {
      tui.send(navigation)?;
      tui
        .wait_screen("prefix navigation selects the pane", |screen| {
          // A six-second disconnect notice can hide the pane number. The
          // live cursor proves focus without waiting for that notice to expire.
          screen.cursor_visible
            && screen.cursor.1 < usize::from(ROWS - 1)
            && (screen.cursor.0 < usize::from(COLUMNS / 2)) == (pane == 1)
        })
        .await?;
      let marker = format!("cycle-{cycle}-{tag}");
      tui.send(format!("{marker}\r").as_bytes())?;
      let expected = format!("{tag}:{marker}");
      tui
        .wait_screen("the persistent shell receives actual input", |screen| {
          screen.contains(&expected)
        })
        .await?;
    }
  }
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_keeps_shared_zoom_hidden_panes_and_frozen_copy_selection() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = split_session(&daemon, "frozen-reconnect").await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  wait_split(&mut tui).await?;
  // Select the first three characters of the first ready line, then zoom
  // while keeping the frozen copy view. The later paste proves its anchor
  // and cursor survived, in addition to its visible rows.
  tui.send(b"\x02[gvll\x02z")?;
  let frozen = tui
    .wait_screen("zoomed frozen selection", |screen| {
      footer(screen).contains("ZOOM")
        && footer(screen).contains("COPY")
        && !screen.contains("second:ready")
        && screen.cursor == (2, 0)
    })
    .await?;
  let rows = &frozen.rows[..usize::from(ROWS - 1)];
  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  tui
    .wait_screen("frozen zoom stays visible while reconnecting", |screen| {
      footer(screen).starts_with(" reconnecting |")
        && footer(screen).contains("ZOOM")
        && footer(screen).contains("COPY")
        && screen.rows[..usize::from(ROWS - 1)] == *rows
    })
    .await?;
  proxy.resume();
  proxy.wait_attachments(before + 2).await?;
  tui
    .wait_screen("confirmed reconnect preserves the frozen zoom", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("ZOOM")
        && footer(screen).contains("COPY")
        && screen.rows[..usize::from(ROWS - 1)] == *rows
        && screen.cursor == frozen.cursor
    })
    .await?;
  tui.send(b"y")?;
  tui
    .wait_screen("the retained selection copies successfully", |screen| {
      footer(screen).contains("Copied to ctmux buffer")
    })
    .await?;
  tui.send(b"\x02]\r")?;
  tui
    .wait_screen(
      "pasting the retained selection reaches the shell",
      |screen| screen.contains("first:fir"),
    )
    .await?;
  tui.send(b"\x02z\x02\x1b[C")?;
  wait_split(&mut tui).await?;
  tui.send(b"hidden-after-reconnect\r")?;
  tui
    .wait_screen("the hidden pane retained its input attachment", |screen| {
      screen.contains("second:hidden-after-reconnect")
    })
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_refresh_handshake_keeps_local_help_and_live_input_responsive() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("stalled-request", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("live fixture ready", |screen| {
      screen.contains("first:ready")
    })
    .await?;
  proxy.stall_requests();
  proxy.wait_stalled(0).await?;

  // This starts only after receipt of a blocked request's handshake. The
  // transport remains stalled until both local controls and live pane input
  // are proved usable; waiting for network release would hide the regression.
  timeout(Duration::from_secs(1), help(&mut tui))
    .await
    .map_err(|_| {
      format!(
        "stalled refresh blocked local help\n{}",
        tui.screen().diagnostic()
      )
    })??;
  tui.send(b"while-refresh-stalled\r")?;
  timeout(
    Duration::from_secs(1),
    tui.wait_screen("live input during the stalled refresh", |screen| {
      screen.contains("first:while-refresh-stalled")
    }),
  )
  .await
  .map_err(|_| {
    format!(
      "stalled refresh blocked shell input\n{}",
      tui.screen().diagnostic()
    )
  })??;
  proxy.resume();
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_reconnect_tokens_do_not_reclaim_explicitly_released_leases() -> Result<()> {
  let mut daemon = TestDaemon::start_with_liveness(Duration::from_millis(500)).await?;
  let session = daemon
    .create_echo_session("released-reconnect", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  tui.send(b"\x02I\x02R")?;
  tui
    .wait_screen("explicit input and resize releases", |screen| {
      footer(screen).contains("view only") && footer(screen).contains("shared size")
    })
    .await?;

  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  // Exercise the daemon's bounded token lifetime. The recorded rejection
  // below, rather than elapsed time alone, proves fresh-attachment fallback.
  tokio::time::sleep(Duration::from_secs(2)).await;
  proxy.resume();
  proxy.wait_attachments(before + 1).await?;
  assert!(
    proxy.resume_rejections() > 0,
    "resume token must have expired"
  );
  tui
    .wait_screen("released ownership survives fresh fallback", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("view only")
        && footer(screen).contains("shared size")
    })
    .await?;

  tui.send(b"\x02I\x02R")?;
  tui
    .wait_screen("manual ownership acquisition remains usable", |screen| {
      footer(screen).contains("input") && footer(screen).contains("resize owner")
    })
    .await?;
  tui.send(b"after-manual-reacquire\r")?;
  tui
    .wait_screen(
      "input reaches the persistent shell after reacquiring",
      |screen| screen.contains("first:after-manual-reacquire"),
    )
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lease_releases_during_reconnect_apply_to_the_preserved_attachment() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("release-during-reconnect", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  tui
    .wait_screen("a disconnected pane with its leases preserved", |screen| {
      footer(screen).starts_with(" reconnecting |")
    })
    .await?;
  tui.send(b"\x02I\x02R")?;
  // Processing and drawing help after both releases is a local input barrier.
  // The replacement handshake stays held until that barrier has completed.
  help(&mut tui).await?;
  proxy.resume();
  proxy.wait_attachments(before + 1).await?;
  assert_eq!(proxy.resume_rejections(), 0, "the token must remain valid");
  assert_eq!(proxy.resumptions(), 1, "the logical attachment must resume");
  tui
    .wait_screen_for(
      Duration::from_secs(8),
      "reconnect applies input and resize releases from the outage",
      |screen| {
        footer(screen).starts_with(" connected |")
          && footer(screen).contains("view only")
          && footer(screen).contains("shared size")
      },
    )
    .await?;
  tui.send(b"denied-during-resume\r")?;
  tui
    .wait_screen("released input is rejected locally", |screen| {
      footer(screen).contains("does not own the input lease")
        && !screen.contains("first:denied-during-resume")
    })
    .await?;
  tui.send(b"\x02I\x02R")?;
  tui
    .wait_screen_for(
      Duration::from_secs(8),
      "manual reacquisition restores ownership after the denial notice",
      |screen| {
        footer(screen).starts_with(" connected |")
          && footer(screen).contains("input")
          && footer(screen).contains("resize owner")
      },
    )
    .await?;
  tui.send(b"after-outage-reacquire\r")?;
  tui
    .wait_screen(
      "the persistent shell accepts input after manual acquisition",
      |screen| {
        screen.contains("first:after-outage-reacquire")
          && !screen.contains("first:denied-during-resume")
      },
    )
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeating_offline_lease_toggles_cancels_the_queued_releases() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("cancel-offline-release", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  tui
    .wait_screen(
      "disconnected pane ready for local ownership toggles",
      |screen| footer(screen).starts_with(" reconnecting |"),
    )
    .await?;
  // The first pair queues release. The second pair cancels that intent before
  // a replacement transport can apply either operation on the daemon.
  tui.send(b"\x02I\x02R\x02I\x02R")?;
  help(&mut tui).await?;
  proxy.resume();
  proxy.wait_attachments(before + 1).await?;
  assert_eq!(proxy.resume_rejections(), 0);
  assert_eq!(proxy.resumptions(), 1);
  tui
    .wait_screen("cancelled releases preserve both owned leases", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("input")
        && footer(screen).contains("resize owner")
    })
    .await?;
  tui.send(b"after-cancelled-release\r")?;
  tui
    .wait_screen(
      "input stays usable after cancelling offline releases",
      |screen| screen.contains("first:after-cancelled-release"),
    )
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manual_lease_acquisition_during_reconnect_applies_after_valid_resume() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("acquire-during-reconnect", "first", canvas())
    .await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  tui.send(b"\x02I\x02R")?;
  tui
    .wait_screen("both leases are unheld before the interruption", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("view only")
        && footer(screen).contains("shared size")
    })
    .await?;
  let before = proxy.attachments();
  proxy.interrupt();
  proxy.wait_stalled(0).await?;
  tui
    .wait_screen("a disconnected view-only pane", |screen| {
      footer(screen).starts_with(" reconnecting |")
    })
    .await?;
  // These are explicit acquisition requests during the outage, independently
  // of the initial attachment's default request to own an available lease.
  tui.send(b"\x02I\x02R")?;
  help(&mut tui).await?;
  proxy.resume();
  proxy.wait_attachments(before + 1).await?;
  assert_eq!(proxy.resume_rejections(), 0);
  assert_eq!(proxy.resumptions(), 1);
  tui
    .wait_screen("resume applies both queued acquisitions", |screen| {
      footer(screen).starts_with(" connected |")
        && footer(screen).contains("input")
        && footer(screen).contains("resize owner")
    })
    .await?;
  tui.send(b"after-offline-acquire\r")?;
  tui
    .wait_screen("queued acquisition restores actual shell input", |screen| {
      screen.contains("first:after-offline-acquire")
    })
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}
