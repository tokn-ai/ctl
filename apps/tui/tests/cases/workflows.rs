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
        "modified prefix key is consumed and input stays in pane one",
        |screen| screen.contains(&expected),
      )
      .await?;
    assert!(!screen.contains(&format!("second:{marker}")));
  }

  // A real unmodified prefix command still works after each unknown binding.
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
