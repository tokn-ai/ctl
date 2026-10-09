use crate::support::{Result, Screen, TestDaemon, TestProxy, Tui};
use ctmux_proto::{
  ClientMessage, CommandSpec, DEFAULT_PRESENTATION_WINDOW_BYTES, ServerMessage, SplitAxis,
  TerminalSize, ViewInfo,
};
use std::{fmt::Write as _, time::Duration};
use tokio::time::{Instant, sleep};

const COLUMNS: u16 = 120;
const ROWS: u16 = 20;
const PREFIX: &[u8] = b"\x1b[98;5u";
const CTRL_I: &[u8] = b"\x1b[105;5u";
const CTRL_SHIFT_A: &[u8] = b"\x1b[97:65;6u";
const ALT_SHIFT_1: &[u8] = b"\x1b[49:33;4u";
const CTRL_ENTER: &[u8] = b"\x1b[13;5u";
const ESCAPE: &[u8] = b"\x1b[27u";

fn canvas() -> TerminalSize {
  TerminalSize {
    columns: COLUMNS,
    rows: ROWS - 1,
    ..TerminalSize::default()
  }
}

fn footer(screen: &Screen) -> &str {
  screen.rows.last().map_or("", String::as_str).trim_end()
}

/// Read exact bytes from the child PTY, preserving control bytes and carriage
/// returns. Each report is a barrier before the next host key is injected.
fn key_fixture(tag: &str, mode: u8, steps: &[(&str, &[u8])]) -> CommandSpec {
  let mut script = format!(
    "PATH=/usr/bin:/bin; export PATH; stty raw -echo; printf '\\033[>4;{mode}m'; printf '%s:ready\\r\\n' \"$1\";"
  );
  for (index, (before, expected)) in steps.iter().enumerate() {
    script.push_str(before);
    write!(
      script,
      "printf '%s:{}:' \"$1\"; dd bs=1 count={} 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '\\r\\n';",
      index + 1,
      expected.len(),
    )
    .expect("writing to a String cannot fail");
  }
  script.push_str("printf '%s:done\\r\\n' \"$1\"; IFS= read -r line");
  CommandSpec {
    program: "/bin/sh".into(),
    arguments: vec!["-c".into(), script, "ctmux-key-fixture".into(), tag.into()],
  }
}

async fn create_fixture(daemon: &TestDaemon, command: CommandSpec) -> Result<String> {
  match daemon
    .request(ClientMessage::CreateSession {
      name: Some("extended-keys".into()),
      command: Some(command),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(),
    })
    .await?
  {
    ServerMessage::SessionCreated { session } => Ok(session.session_id),
    response => Err(format!("expected keyboard fixture, received {response:?}").into()),
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
    response => Err(format!("expected keyboard fixture view, got {response:?}").into()),
  }
}

async fn wait_checkpoint(daemon: &TestDaemon, terminal: &str, tag: &str, mode: u8) -> Result<()> {
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    // This spectator attachment requests no leases and proves the daemon has
    // processed the mode request before the real TUI is opened.
    let response = daemon
      .request(ClientMessage::AttachSession {
        session: terminal.into(),
        resume_from: None,
        terminal_size: canvas(),
        request_input_lease: false,
        request_layout_lease: false,
        request_command_line: false,
        request_running_command: false,
        presentation_window_bytes: DEFAULT_PRESENTATION_WINDOW_BYTES,
      })
      .await?;
    if let ServerMessage::Attached {
      checkpoint: Some(checkpoint),
      ..
    } = response
    {
      let payload = std::str::from_utf8(&checkpoint.payload)?;
      if payload.contains(&format!("{tag}:ready")) {
        if mode != 0 {
          assert!(
            payload.contains(&format!("\x1b[>4;{mode}m")),
            "keyboard mode was omitted from the initial checkpoint"
          );
        }
        return Ok(());
      }
    }
    if Instant::now() >= deadline {
      return Err(format!("keyboard fixture {tag} did not produce its initial checkpoint").into());
    }
    sleep(Duration::from_millis(10)).await;
  }
}

async fn expect_bytes(
  tui: &mut Tui,
  tag: &str,
  step: usize,
  host_input: Option<&[u8]>,
  expected: &[u8],
) -> Result<()> {
  let prompt = format!("{tag}:{step}:");
  tui
    .wait_screen("child is ready for a key packet", |screen| {
      screen.contains(&prompt)
    })
    .await?;
  if let Some(input) = host_input {
    tui.send(input)?;
  }
  let hexadecimal = expected.iter().fold(String::new(), |mut output, byte| {
    write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    output
  });
  let report = format!("{prompt}{hexadecimal}");
  tui
    .wait_screen("exact key packet reaches the child PTY", |screen| {
      screen.contains(&report)
    })
    .await?;
  Ok(())
}

async fn detach(tui: &mut Tui) -> Result<()> {
  tui.send(PREFIX)?;
  tui.send(b"d")?;
  assert!(tui.wait_exit().await?.success());
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_keys_restore_requested_modes_and_preserve_legacy_opt_out() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let steps: [(&str, &[u8]); 13] = [
    ("", b"\x1b[27;5;105~"),
    ("", b"\t"),
    ("", b"\x1b[27;6;65~"),
    ("", b"\x1b[27;5;13~"),
    ("", b"\x1b[27;4;33~"),
    ("", b"\x1b[27;6;201~"),
    ("", b"\x1b[27;6;97~"),
    ("", b"A"),
    ("printf '\\033[?4m';", b"\x1b[>4;2m"),
    ("printf '\\033[>4;0m';", b"\t\x01"),
    ("", b"\x1b!"),
    ("printf '\\033[>4;1m';", b"\t"),
    ("", b"\x1b[27;5;13~"),
  ];
  let session = create_fixture(&daemon, key_fixture("keys", 2, &steps)).await?;
  wait_checkpoint(&daemon, &session, "keys", 2).await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  for (index, host_input) in [
    Some(CTRL_I),
    Some(b"\t".as_slice()),
    Some(CTRL_SHIFT_A),
    Some(CTRL_ENTER),
    Some(ALT_SHIFT_1),
    Some(b"\x1b[233:201;6u".as_slice()),
    Some(b"\x1b[97:97;70u".as_slice()),
    Some(b"A".as_slice()),
    None,
    Some(b"\x1b[105;5u\x1b[97:65;6u".as_slice()),
    Some(ALT_SHIFT_1),
    Some(CTRL_I),
    Some(CTRL_ENTER),
  ]
  .into_iter()
  .enumerate()
  {
    expect_bytes(&mut tui, "keys", index + 1, host_input, steps[index].1).await?;
  }
  detach(&mut tui).await?;
  tui
    .wait_transcript("host reports disambiguated and shifted keys", b"\x1b[>5u")
    .await?;
  tui
    .wait_transcript(
      "detach restores the host keyboard enhancement stack",
      b"\x1b[<1u",
    )
    .await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}

async fn focus(tui: &mut Tui, right: bool) -> Result<()> {
  tui.send(PREFIX)?;
  tui.send(if right { b"\x1b[C" } else { b"\x1b[D" })?;
  tui
    .wait_screen("prefix selects the keyboard fixture pane", |screen| {
      if right {
        screen.cursor.0 > screen.columns / 2
      } else {
        screen.cursor.0 < screen.columns / 2
      }
    })
    .await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_keys_stay_pane_local_across_resize_reconnect_and_reattach() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let enhanced: [(&str, &[u8]); 4] = [
    ("", b"\x1b[27;5;105~"),
    ("", b"\x1b[27;6;65~"),
    ("", b"\x1b[27;5;13~"),
    ("", b"\x1b[27;5;105~"),
  ];
  let legacy: [(&str, &[u8]); 4] = [("", b"\t"), ("", b"\x01"), ("", b"\r"), ("", b"\t")];
  let session = create_fixture(&daemon, key_fixture("a", 2, &enhanced)).await?;
  let initial = view(&daemon, &session).await?;
  let primary = initial.terminals[0].terminal_id.clone();
  let ServerMessage::ViewSnapshot { view: split } = daemon
    .request(ClientMessage::SplitTerminal {
      terminal_id: primary.clone(),
      axis: SplitAxis::Horizontal,
      command: Some(key_fixture("b", 0, &legacy)),
      working_directory: Some(daemon.directory.to_string_lossy().into_owned()),
      terminal_size: canvas(),
    })
    .await?
  else {
    return Err("expected split keyboard fixture".into());
  };
  let child = split
    .terminals
    .iter()
    .find(|terminal| terminal.terminal_id != primary)
    .ok_or("split fixture has no second terminal")?
    .terminal_id
    .clone();
  wait_checkpoint(&daemon, &primary, "a", 2).await?;
  wait_checkpoint(&daemon, &child, "b", 0).await?;
  let proxy = TestProxy::start(&daemon)?;
  let mut tui = Tui::start_socket(&daemon, &proxy.socket, &session, COLUMNS, ROWS).await?;
  for (index, key) in [CTRL_I, CTRL_SHIFT_A, CTRL_ENTER, CTRL_I]
    .into_iter()
    .enumerate()
  {
    if index == 1 {
      tui.resize(100, 16)?;
      tui
        .wait_screen("resized host restores its pane models", |screen| {
          screen.columns == 100
            && screen.rows.len() == 16
            && footer(screen).starts_with(" connected |")
        })
        .await?;
    } else if index == 2 {
      let attachments = proxy.attachments();
      let stalled = proxy.stalled();
      proxy.interrupt();
      proxy.wait_stalled(stalled).await?;
      proxy.resume();
      proxy.wait_attachments(attachments + 2).await?;
      tui
        .wait_screen(
          "the same TUI reconnects both keyboard-mode panes",
          |screen| footer(screen).starts_with(" connected |"),
        )
        .await?;
    } else if index == 3 {
      detach(&mut tui).await?;
      drop(tui);
      tui = Tui::start_socket(&daemon, &proxy.socket, &session, 100, 16).await?;
    }
    focus(&mut tui, false).await?;
    expect_bytes(&mut tui, "a", index + 1, Some(key), enhanced[index].1).await?;
    focus(&mut tui, true).await?;
    expect_bytes(&mut tui, "b", index + 1, Some(key), legacy[index].1).await?;
  }
  detach(&mut tui).await?;
  drop(tui);
  drop(proxy);
  daemon.shutdown().await?;
  Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extended_host_keys_keep_prefix_prompt_and_copy_controls_local() -> Result<()> {
  let mut daemon = TestDaemon::start().await?;
  let session = create_fixture(&daemon, key_fixture("local", 2, &[("", b"z")])).await?;
  wait_checkpoint(&daemon, &session, "local", 2).await?;
  let mut tui = Tui::start(&daemon, &session, COLUMNS, ROWS).await?;
  tui.send(PREFIX)?;
  tui.send(b":unsubmitted")?;
  tui
    .wait_screen("CSI-u prefix opens the local command prompt", |screen| {
      footer(screen) == ":unsubmitted"
    })
    .await?;
  tui.send(ESCAPE)?;
  tui
    .wait_screen("CSI-u Escape cancels the local prompt", |screen| {
      !footer(screen).starts_with(':')
    })
    .await?;
  tui.send(PREFIX)?;
  tui.send(b"[g\x1b[102;5u")?;
  tui
    .wait_screen("CSI-u Ctrl+F moves inside frozen copy mode", |screen| {
      footer(screen).contains("COPY") && screen.cursor == (1, 0)
    })
    .await?;
  tui.send(ESCAPE)?;
  tui
    .wait_screen("CSI-u Escape returns from copy mode", |screen| {
      !footer(screen).contains("COPY")
    })
    .await?;
  // A one-byte capture proves none of the local keys leaked into the child.
  expect_bytes(&mut tui, "local", 1, Some(b"z"), b"z").await?;
  detach(&mut tui).await?;
  drop(tui);
  daemon.shutdown().await?;
  Ok(())
}
