use crate::support::{Result, Screen, TestDaemon, Tui};
use ctmux_proto::{ClientMessage, PaneGeometry, ServerMessage, SplitAxis, TerminalSize, ViewInfo};
use std::fmt::Write as _;
use std::time::Duration;

const COLUMNS: u16 = 160;
const ROWS: u16 = 25;

fn canvas_size() -> TerminalSize {
  TerminalSize {
    columns: COLUMNS,
    rows: ROWS - 1,
    ..TerminalSize::default()
  }
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_decoding_repeats_pane_arrows_and_returns_to_ordinary_input() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("navigation", "first", canvas_size())
    .await?;
  let first = view(&daemon, &session).await?.panes[0].terminal_id.clone();
  let split = daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", canvas_size())
    .await?;
  let second = split
    .panes
    .iter()
    .find(|pane| pane.terminal_id != first)
    .ok_or("split did not create a second pane")?
    .terminal_id
    .clone();
  daemon
    .split_echo(&second, SplitAxis::Horizontal, "third", canvas_size())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("all three panes ready with the first focused", |screen| {
      screen.contains("first:ready")
        && screen.contains("second:ready")
        && screen.contains("third:ready")
        && footer(screen).contains("pane 1")
    })
    .await?;

  // One terminal write keeps the second arrow inside the repeat window.
  // Neither `d` nor `o` is a repeatable command; both must become PTY input.
  tui.send(b"\x02\x1b[C\x1b[Cdo\r")?;
  tui
    .wait_screen(
      "repeated arrows select pane three and typing stays there",
      |screen| footer(screen).contains("pane 3") && screen.contains("third:do"),
    )
    .await?;

  tui.send(b"\x02\x1b[D")?;
  tui
    .wait_screen("a fresh prefix selects pane two", |screen| {
      footer(screen).contains("pane 2")
    })
    .await?;
  // This intentionally tests elapsed terminal time, with a generous margin
  // above the 500 ms repeat deadline; exact boundaries have fast unit tests.
  tokio::time::sleep(Duration::from_millis(750)).await;
  // Discard the line containing the literal cursor escape before the marker.
  tui.send(b"\x1b[Ddiscard\rafter-expiry\r")?;
  tui
    .wait_screen(
      "expired arrows become input without moving pane focus",
      |screen| footer(screen).contains("pane 2") && screen.contains("second:after-expiry"),
    )
    .await?;

  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modified_terminal_keys_do_not_alias_detach_or_plain_pane_arrows() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("modifiers", "first", canvas_size())
    .await?;
  let first = view(&daemon, &session).await?.panes[0].terminal_id.clone();
  daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", canvas_size())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("both modifier test panes ready", |screen| {
      screen.contains("first:ready")
        && screen.contains("second:ready")
        && footer(screen).contains("pane 1")
    })
    .await?;

  for (bytes, marker) in [
    (b"\x02\x04ctrl-d\r".as_slice(), "ctrl-d"),
    (b"\x02\x1b[1;5Cctrl-right\r".as_slice(), "ctrl-right"),
    (b"\x02\x1b[1;3Calt-right\r".as_slice(), "alt-right"),
  ] {
    tui.send(bytes)?;
    let expected = format!("first:{marker}");
    let screen = tui
      .wait_screen(
        "modified prefix key preserves focus and input stays in pane one",
        |screen| screen.contains(&expected),
      )
      .await?;
    assert!(!screen.contains(&format!("second:{marker}")));
  }

  // Control/Alt arrows resize rather than aliasing focus; Ctrl+D remains unbound.
  // The unmodified detach command still works after those modified keys.
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_copy_and_mouse_scrolling_keep_the_status_on_the_bottom_row() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("history", "first", canvas_size())
    .await?;
  let first = view(&daemon, &session).await?.panes[0].terminal_id.clone();
  let split = daemon
    .split_echo(&first, SplitAxis::Horizontal, "second", canvas_size())
    .await?;
  let first_geometry = split.panes[0].clone();
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("history panes ready", |screen| {
      screen.contains("first:ready") && screen.contains("second:ready")
    })
    .await?;

  let lines = (0..36).fold(String::new(), |mut lines, line| {
    writeln!(lines, "line-{line:02}").expect("write to string");
    lines
  });
  tui.send(format!("\x1b[200~{lines}\x1b[201~").as_bytes())?;
  let live = tui
    .wait_screen("bracketed paste produced scrollback", |screen| {
      screen.contains("first:line-35") && footer(screen).contains("pane 1")
    })
    .await?;
  assert!(!live.contains("first:line-00"));

  tui.send(b"\x02[\x1b[5~\x1b[5~")?;
  let copied = tui
    .wait_screen(
      "Page Up exposes older output with a fixed copy footer",
      |screen| screen.contains("first:line-00") && footer(screen).contains("COPY"),
    )
    .await?;
  assert!(footer(&copied).contains("connected"));
  assert!(
    copied.rows[..usize::from(ROWS - 1)]
      .iter()
      .all(|row| !row.contains("COPY"))
  );
  let frozen = pane_rows(&copied, &first_geometry);

  tui.send(b"\x02\x1b[Cother-pane-live\r")?;
  let other = tui
    .wait_screen("the other pane continues accepting input", |screen| {
      screen.contains("second:other-pane-live") && footer(screen).contains("pane 2")
    })
    .await?;
  assert_eq!(pane_rows(&other, &first_geometry), frozen);
  tui.send(b"\x02\x1b[D")?;
  let returned = tui
    .wait_screen(
      "returning to the first pane restores its copy status",
      |screen| footer(screen).contains("COPY") && screen.contains("first:line-00"),
    )
    .await?;
  assert_eq!(pane_rows(&returned, &first_geometry), frozen);

  tui.resize(COLUMNS, 18)?;
  let resized = tui
    .wait_screen(
      "host PTY resize moves the copy footer to the new bottom",
      |screen| {
        screen.rows.len() == 18
          && footer(screen).contains("COPY")
          && screen.contains("first:line-00")
      },
    )
    .await?;
  assert!(resized.rows[..17].iter().all(|row| !row.contains("COPY")));

  tui.send(b"q")?;
  tui
    .wait_screen("leaving copy mode returns to current output", |screen| {
      !footer(screen).contains("COPY") && screen.contains("first:line-35")
    })
    .await?;
  // SGR wheel reports exercise terminal mouse decoding, not direct event injection.
  tui.send(b"\x1b[<64;2;2M")?;
  tui
    .wait_screen(
      "wheel up enters history without scrolling the footer",
      |screen| footer(screen).contains("COPY"),
    )
    .await?;
  tui.send(b"\x1b[<65;2;2M")?;
  tui
    .wait_screen(
      "wheel down at the bottom returns to live output",
      |screen| !footer(screen).contains("COPY") && screen.contains("first:line-35"),
    )
    .await?;

  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mouse_selection_waits_for_explicit_copy_and_keeps_the_scrolled_view() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = daemon
    .create_echo_session("explicit-copy", "first", canvas_size())
    .await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui
    .wait_screen("selection fixture ready", |screen| {
      screen.contains("first:ready")
    })
    .await?;
  let lines = (0..36).fold(String::new(), |mut lines, line| {
    writeln!(lines, "line-{line:02}").expect("write to string");
    lines
  });
  tui.send(format!("\x1b[200~{lines}\x1b[201~").as_bytes())?;
  tui
    .wait_screen("selection fixture produced history", |screen| {
      screen.contains("first:line-35")
    })
    .await?;
  tui.send(b"\x02[g")?;
  let original = tui
    .wait_screen("oldest history is visible", |screen| {
      footer(screen).contains("COPY") && screen.row(0).starts_with("first:ready")
    })
    .await?;
  // The release moves beyond the last drag event, so its cursor proves the
  // button-up event was processed before checking for unwanted clipboard output.
  tui.send(b"\x1b[<0;1;1M\x1b[<32;4;1M\x1b[<0;5;1m")?;
  let selected = tui
    .wait_screen("mouse release keeps the selection in history", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (4, 0)
    })
    .await?;
  assert_eq!(
    selected.rows[..usize::from(ROWS - 1)],
    original.rows[..usize::from(ROWS - 1)]
  );
  assert!(!tui.transcript_contains(b"\x1b]52;"));
  tui.send(b"\x1b[<65;2;2M")?;
  let scrolled = tui
    .wait_screen(
      "scrolling moves away from the completed selection",
      |screen| footer(screen).contains("COPY") && screen.row(0) != selected.row(0),
    )
    .await?;
  assert!(!tui.transcript_contains(b"\x1b]52;"));
  tui.send(b"y")?;
  tui
    .wait_transcript(
      "explicit copy sends the original selection",
      b"\x1b]52;c;Zmlyc3Q=\x07",
    )
    .await?;
  let copied = tui
    .wait_screen("explicit copy stays at the scrolled position", |screen| {
      footer(screen).contains("COPY") && footer(screen).contains("Copied to ctmux buffer")
    })
    .await?;
  assert_eq!(
    copied.rows[..usize::from(ROWS - 1)],
    scrolled.rows[..usize::from(ROWS - 1)]
  );
  assert_eq!(copied.cursor, scrolled.cursor);
  tui.send(b"q\x02]\r")?;
  tui
    .wait_screen(
      "explicit exit restores live input and the copy buffer",
      |screen| !footer(screen).contains("COPY") && screen.contains("first:first"),
    )
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selection_config_override_can_copy_on_release_and_return_to_live() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let default_directory = daemon.directory.join("home/.tokn/ctl");
  std::fs::create_dir_all(&default_directory)?;
  std::fs::write(default_directory.join("ctmux.json"), "{}")?;
  let config = daemon.directory.join("selection-override.json");
  std::fs::write(
    &config,
    r#"{"selection":{"mouse_release":"copy","after_copy":"exit-copy-mode"}}"#,
  )?;
  let session = daemon
    .create_echo_session("configured-copy", "first", canvas_size())
    .await?;
  let mut tui =
    Tui::start_socket_with_config(&daemon, &daemon.socket, &session, COLUMNS, ROWS, &config)
      .await?;
  tui
    .wait_screen("configured selection fixture ready", |screen| {
      screen.row(0).starts_with("first:ready")
    })
    .await?;
  tui.send(b"\x1b[<0;1;1M\x1b[<32;4;1M")?;
  tui
    .wait_screen("drag selects without copying early", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (3, 0)
    })
    .await?;
  assert!(!tui.transcript_contains(b"\x1b]52;"));
  tui.send(b"\x1b[<0;5;1m")?;
  tui
    .wait_transcript(
      "configured release copies selection",
      b"\x1b]52;c;Zmlyc3Q=\x07",
    )
    .await?;
  tui
    .wait_screen("configured release exits history", |screen| {
      !footer(screen).contains("COPY") && screen.contains("first:ready")
    })
    .await?;
  tui.send(b"\x02]\r")?;
  tui
    .wait_screen("configured release resumes live input", |screen| {
      screen.contains("first:first")
    })
    .await?;
  tui.send(b"\x02d")?;
  assert!(tui.wait_exit().await?.success());
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_selection_config_fails_before_creating_a_session() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let config = daemon.directory.join("invalid-settings.json");
  std::fs::write(&config, r#"{"selection":{"mouse_release":"paste"}}"#)?;
  let program = option_env!("CARGO_BIN_EXE_ctmux-tui").ok_or("missing TUI binary")?;
  let mut command = portable_pty::CommandBuilder::new(program);
  command.arg("--socket");
  command.arg(&daemon.socket);
  command.env_clear();
  command.env("HOME", daemon.directory.join("home"));
  command.env("PATH", "/usr/bin:/bin");
  command.env("SHELL", "/bin/sh");
  command.env("TERM", "xterm-256color");
  command.env("CTMUX_CONFIG_FILE", &config);
  command.cwd(&daemon.directory);
  let mut tui = Tui::spawn(command, COLUMNS, ROWS)?;
  assert!(!tui.wait_exit().await?.success());
  tui
    .wait_transcript(
      "configuration error identifies the file",
      b"invalid-settings.json",
    )
    .await?;
  assert!(tui.transcript_contains(b"could not load ctmux settings"));
  assert!(matches!(
    daemon.request(ClientMessage::ListSessions).await?,
    ServerMessage::SessionList { sessions } if sessions.is_empty()
  ));
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}
