#![cfg(unix)]

#[path = "cases/cleanup.rs"]
mod cleanup;
#[path = "cases/commands.rs"]
mod commands;
#[path = "cases/reconnect.rs"]
mod reconnect;
#[path = "cases/resizing.rs"]
mod resizing;
mod support;
#[path = "cases/workflows.rs"]
mod workflows;
#[path = "cases/zoom.rs"]
mod zoom;

use ctmux_proto::{ClientMessage, ServerMessage, SessionStatus, TerminalSize};
use portable_pty::CommandBuilder;
use support::{Result, TestDaemon, Tui};

async fn check_shutdown(disconnect: bool) -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session(
      "shutdown",
      "shell",
      TerminalSize {
        columns: 80,
        rows: 24,
        ..TerminalSize::default()
      },
    )
    .await?;
  let mut tui = Tui::start(&daemon, &session, 80, 25).await?;
  tui
    .wait_screen("shell ready", |screen| screen.contains("shell:ready"))
    .await?;
  if disconnect {
    tui.disconnect()?;
  } else {
    tui.send(b"\x02d")?;
  }
  let status = tui.wait_exit().await?;
  if !disconnect {
    assert!(status.success(), "prefix detach must exit cleanly");
  }
  drop(tui);
  // Losing the host terminal must preserve the daemon's persistent shell PTY.
  let ServerMessage::SessionList { sessions } = daemon.request(ClientMessage::ListSessions).await?
  else {
    return Err("expected session list".into());
  };
  assert_eq!(sessions.len(), 1);
  assert_eq!(sessions[0].session_id, session);
  assert_eq!(sessions[0].status, SessionStatus::Running);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_the_host_terminal_does_not_leave_a_spinning_tui() -> Result<()> {
  check_shutdown(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefix_detach_stops_the_reader_and_preserves_the_session() -> Result<()> {
  check_shutdown(false).await
}

#[tokio::test]
async fn stopped_process_reports_output_without_accepting_it_as_a_live_screen() -> Result<()> {
  let mut command = CommandBuilder::new("/bin/sh");
  command.args(["-c", "printf 'fixture-crash\\r\\n'; exit 7"]);
  let mut tui = Tui::spawn(command, 80, 25)?;
  assert_eq!(tui.wait_exit().await?.exit_code(), 7);
  let error = tui
    .wait_screen("missing live frame", |screen| {
      screen.contains("fixture-crash")
    })
    .await
    .unwrap_err();
  let diagnostic = error.to_string();
  assert!(diagnostic.contains("missing live frame"), "{diagnostic}");
  assert!(diagnostic.contains("fixture-crash"), "{diagnostic}");
  assert!(diagnostic.contains("raw transcript"), "{diagnostic}");
  assert!(tui.transcript_contains(b"fixture-crash"));
  Ok(())
}
