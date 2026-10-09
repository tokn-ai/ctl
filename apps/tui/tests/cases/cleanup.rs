use crate::support::{Result, TestDaemon, Tui};
use ctmux_proto::{ClientMessage, CommandSpec, ServerMessage, TerminalSize};
use nix::{
  errno::Errno,
  sys::signal::{Signal, kill},
  unistd::Pid,
};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::time::{Instant, sleep};

#[tokio::test]
async fn unwinding_reaps_the_tui_and_persistent_fixture_shell() -> Result<()> {
  let daemon = TestDaemon::start().await?;
  let directory = daemon.directory.clone();
  let ServerMessage::SessionCreated { session } = daemon
    .request(ClientMessage::CreateSession {
      name: Some("panic-cleanup".into()),
      command: Some(CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec![
          "-c".into(),
          "printf '%s\\n' \"$$\" > fixture.pid; printf 'cleanup:ready\\n'; IFS= read -r line"
            .into(),
        ],
      }),
      working_directory: Some(directory.to_string_lossy().into_owned()),
      terminal_size: TerminalSize {
        columns: 80,
        rows: 24,
        ..TerminalSize::default()
      },
    })
    .await?
  else {
    return Err("expected cleanup fixture session".into());
  };
  let mut tui = Tui::start(&daemon, &session.session_id, 80, 25).await?;
  tui
    .wait_screen("cleanup shell ready", |screen| {
      screen.contains("cleanup:ready")
    })
    .await?;
  let shell_pid = std::fs::read_to_string(directory.join("fixture.pid"))?
    .trim()
    .parse::<i32>()?;
  let tui_pid = i32::try_from(tui.process_id().ok_or("TUI has no process ID")?)?;

  let unwound = std::panic::catch_unwind(AssertUnwindSafe(move || {
    // Local drop order mirrors a test: close the client, then its daemon.
    let _daemon = daemon;
    let _tui = tui;
    // Exercise unwinding without invoking the expected-panic logging hook.
    std::panic::resume_unwind(Box::new("fixture cleanup probe"));
  }));
  assert!(unwound.is_err());
  assert!(!directory.exists(), "fixture directory survived unwinding");
  wait_until_reaped("TUI", tui_pid).await?;
  wait_until_reaped("fixture shell", shell_pid).await?;
  Ok(())
}

async fn wait_until_reaped(description: &str, process_id: i32) -> Result<()> {
  let deadline = Instant::now() + Duration::from_secs(3);
  loop {
    // Signal zero only checks existence; no signal is sent to the process.
    match kill(Pid::from_raw(process_id), None) {
      Err(Errno::ESRCH) => return Ok(()),
      Err(error) => return Err(format!("could not inspect {description}: {error}").into()),
      Ok(()) if Instant::now() < deadline => sleep(Duration::from_millis(10)).await,
      Ok(()) => {
        return Err(format!("{description} process {process_id} survived unwinding").into());
      }
    }
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handled_signals_restore_host_keyboard_and_screen_modes() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("signal-modes", "shell", TerminalSize::default())
    .await?;
  for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
    let mut tui = Tui::start(&daemon, &session, 80, 25).await?;
    tui
      .wait_transcript("enhanced host keyboard entered", b"\x1b[>5u")
      .await?;
    let process_id = i32::try_from(tui.process_id().ok_or("TUI has no process ID")?)?;
    kill(Pid::from_raw(process_id), signal)?;
    assert!(tui.wait_exit().await?.success(), "{signal:?}");
    tui
      .wait_transcript(
        "keyboard mode popped before screen restoration",
        b"\x1b[<1u\x1b[0m",
      )
      .await?;
    tui
      .wait_transcript("host alternate screen restored", b"\x1b[?1049l")
      .await?;
  }
  daemon.shutdown().await?;
  Ok(())
}
