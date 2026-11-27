#[path = "observer_control.rs"]
mod observer_control;
#[path = "semantic_turn.rs"]
mod semantic_turn;
#[path = "turns.rs"]
mod turns;

use super::{
  host::{HostFixture, TEST_SSH, activity_details},
  wait_for,
};
use ctream_client::{Client, transport::Local};
use ctream_proto::{Target, WindowId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
  collections::HashSet,
  error::Error,
  fs,
  io::{self, BufRead, BufReader, Write},
  path::Path,
  process::{Child, Command, Stdio},
  sync::mpsc,
  thread,
  time::Duration,
};
use uuid::Uuid;
use x11rb::{
  COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
  connection::Connection,
  protocol::{
    Event,
    xproto::{AtomEnum, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass},
  },
  wrapper::ConnectionExt as _,
};

const AGENT: &str = env!("CARGO_BIN_EXE_ctream-agent");
const FIRST_TEXT: &str = "privatecobalt";
const SECOND_TEXT: &str = "privatelemon";
struct NamedClient {
  child: Child,
  output: mpsc::Receiver<io::Result<Value>>,
}

impl NamedClient {
  fn open(
    caller: &Path,
    host: &Path,
    target: &str,
    window: &str,
    command_name: &str,
    identity: Option<(&str, &str)>,
  ) -> io::Result<Self> {
    let mut command = Command::new(Path::new(AGENT).with_file_name("ctream"));
    command
      .env("XDG_STATE_HOME", caller)
      .env("CTREAM_TEST_HOST_STATE", host)
      .args([
        "--host",
        "fixture-display",
        "--ssh-program",
        "sh",
        "--ssh-arg",
        "-c",
        "--ssh-arg",
        TEST_SSH,
        "--ssh-arg",
        "fixture-ssh",
        "--agent",
        AGENT,
        command_name,
        target,
        "--window",
        window,
      ]);
    if let Some((protocol, agent_id)) = identity {
      command.args(["--protocol", protocol, "--agent-id", agent_id]);
    }
    let mut child = command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()?;
    let reader = BufReader::new(child.stdout.take().unwrap());
    let (sender, output) = mpsc::channel();
    thread::spawn(move || {
      for line in reader.lines() {
        let event = line.and_then(|line| Ok(serde_json::from_str(&line)?));
        if sender.send(event).is_err() {
          break;
        }
      }
    });
    Ok(Self { child, output })
  }

  fn send(&mut self, request: Value) -> io::Result<()> {
    let input = self.child.stdin.as_mut().unwrap();
    serde_json::to_writer(&mut *input, &request)?;
    input.write_all(b"\n")?;
    input.flush()
  }

  fn receive(&mut self) -> io::Result<Value> {
    self
      .output
      .recv_timeout(Duration::from_secs(10))
      .map_err(io::Error::other)?
  }

  fn close(mut self) -> io::Result<()> {
    drop(self.child.stdin.take());
    assert!(
      self.child.wait()?.success(),
      "named session failed to clean up on EOF"
    );
    Ok(())
  }
}

impl Drop for NamedClient {
  fn drop(&mut self) {
    drop(self.child.stdin.take());
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Activity {
  action_id: String,
  session_id: String,
  agent_id: String,
  timestamp_ms: u64,
  target: String,
  window_id: String,
  operation: String,
  status: String,
}

fn activity(state: &Path) -> io::Result<Vec<Activity>> {
  let output = Command::new(AGENT)
    .args(["activity", "--limit", "10"])
    .env("XDG_STATE_HOME", state)
    .output()?;
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  Ok(serde_json::from_slice(&output.stdout)?)
}

fn fixture_window(
  conn: &x11rb::rust_connection::RustConnection,
  screen: &x11rb::protocol::xproto::Screen,
  target: Target,
  x: i16,
  title: &[u8],
) -> Result<(u32, String), Box<dyn Error>> {
  let fixture = conn.generate_id()?;
  conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      fixture,
      screen.root,
      x,
      30,
      220,
      100,
      0,
      WindowClass::INPUT_OUTPUT,
      COPY_FROM_PARENT,
      &CreateWindowAux::new()
        .background_pixel(screen.white_pixel)
        .event_mask(EventMask::KEY_PRESS),
    )?
    .check()?;
  conn
    .change_property8(
      PropMode::REPLACE,
      fixture,
      AtomEnum::WM_NAME,
      AtomEnum::STRING,
      title,
    )?
    .check()?;
  conn.map_window(fixture)?.check()?;
  let client = Client::new(Local {
    agent: AGENT.into(),
  });
  let window = wait_for("named fixture window", || {
    Ok(
      client
        .windows(target)?
        .into_iter()
        .find(|w| WindowId::x11_xid(&w.id) == Some(fixture)),
    )
  })?
  .id;
  Ok((fixture, window))
}

#[test]
#[ignore = "requires the disposable X11 session and sibling CLI in docker/ctream"]
fn two_named_clients_attribute_physical_input_to_the_same_window() -> Result<(), Box<dyn Error>> {
  let fixture = HostFixture::new("attribution")?;
  let host = fixture.state();
  let caller_a = fixture.caller("a")?;
  let caller_b = fixture.caller("b")?;
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let (fixture, window) = fixture_window(
    &conn,
    screen,
    Target::parse(&display)?,
    20,
    b"Ctream named fixture",
  )?;
  let mut a = NamedClient::open(&caller_a, host, &display, &window, "session", None)?;
  let mut b = NamedClient::open(
    &caller_b,
    host,
    &display,
    &window,
    "session",
    Some(("1.0.3", "writer-b")),
  )?;

  // Both independently launched clients hold the same live window open.
  for client in [&mut a, &mut b] {
    client.send(json!({"operation": "screenshot"}))?;
    let reply = client.receive()?;
    assert_eq!(reply["result"]["mime_type"], "image/png", "{reply}");
  }
  assert!(
    activity(host)?.is_empty(),
    "observations created mutation records"
  );
  // Independent SSH viewers join after the writers. Each sees the current
  // roster and receives the same redacted operations in host broadcast order.
  let mut observers = Vec::new();
  for (index, caller) in [&caller_a, &caller_b].into_iter().enumerate() {
    let mut viewer = NamedClient::open(
      caller,
      host,
      &display,
      &window,
      "watch",
      Some(("1.0.5", "observer")),
    )?;
    let snapshot = viewer.receive()?;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(
      snapshot["participants"].as_array().unwrap().len(),
      3 + index
    );
    observers.push(viewer);
  }
  assert_eq!(observers[0].receive()?["type"], "joined");
  a.send(json!({"operation":"click","point":{"x":54,"y":24},"size":[220,100],"button":"left"}))?;
  assert_eq!(a.receive()?, json!({"ok":true,"result":null}));
  let click = observers[0].receive()?;
  assert_eq!(click, observers[1].receive()?);
  assert_eq!(click["action"]["operation"], "click");
  for (actual, expected) in click["action"]["point"]
    .as_array()
    .unwrap()
    .iter()
    .zip([54.5 / 220.0, 24.5 / 100.0])
  {
    assert!((actual.as_f64().unwrap() - expected).abs() < 1e-6);
  }
  // Submit both before waiting on either. The physical event order must agree
  // with the host ledger and each full text action must stay contiguous.
  a.send(json!({"operation":"type_text", "text": FIRST_TEXT}))?;
  b.send(json!({"operation":"type_text", "text": SECOND_TEXT}))?;
  for client in [&mut a, &mut b] {
    assert_eq!(client.receive()?, json!({"ok": true, "result": null}));
  }
  let mut authors = HashSet::new();
  for _ in 0..2 {
    let event = observers[0].receive()?;
    assert_eq!(observers[1].receive()?, event);
    assert_eq!(event["type"], "action");
    assert_eq!(event["action"], json!({"operation":"type_text"}));
    authors.insert(event["participant_id"].as_str().unwrap().to_owned());
  }
  assert_eq!(authors.len(), 2);
  a.send(json!({"operation":"click", "point":{"x":220,"y":1}, "size":[220,100], "button":"left"}))?;
  assert_eq!(a.receive()?["ok"], false);
  a.close()?;
  b.close()?;
  let mut departed = HashSet::new();
  for _ in 0..2 {
    let event = observers[0].receive()?;
    assert_eq!(event, observers[1].receive()?);
    assert_eq!(event["type"], "left");
    departed.insert(event["participant_id"].as_str().unwrap().to_owned());
  }
  assert_eq!(departed, authors);
  for viewer in observers {
    viewer.close()?;
  }

  for caller in [&caller_a, &caller_b] {
    assert!(
      activity(caller)?.is_empty(),
      "activity was recorded on a caller"
    );
    assert!(
      fs::read_dir(caller)?.next().is_none(),
      "unexpected caller-local state"
    );
  }
  let rows = activity(host)?;
  assert_eq!(rows.len(), 4);
  assert_eq!(rows[3].session_id, click["participant_id"]);
  assert_eq!(
    (&*rows[3].operation, &*rows[3].status),
    ("click", "succeeded")
  );
  let failed = &rows[0];
  assert_eq!((&*failed.operation, &*failed.status), ("click", "failed"));
  let (word, salt) = failed
    .agent_id
    .split_once('#')
    .expect("generated agent ID contains #");
  assert!(!word.is_empty() && word.bytes().all(|c| c.is_ascii_lowercase()));
  assert_eq!(salt.len(), 4);
  assert!(salt.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
  let mut ids = HashSet::new();
  for row in &rows {
    assert!(ids.insert(Uuid::parse_str(&row.action_id)?));
    Uuid::parse_str(&row.session_id)?;
    assert_eq!(row.target, display);
    assert_eq!(row.window_id, window);
    assert!(row.timestamp_ms > 0);
  }
  assert_ne!(rows[1].agent_id, rows[2].agent_id);
  let mut expected = String::new();
  for row in rows[1..3].iter().rev() {
    assert_eq!((&*row.operation, &*row.status), ("type_text", "succeeded"));
    match row.agent_id.as_str() {
      generated if generated == failed.agent_id => {
        assert_eq!(row.session_id, failed.session_id);
        expected.push_str(FIRST_TEXT);
      }
      "writer-b" => {
        assert_ne!(row.session_id, failed.session_id);
        expected.push_str(SECOND_TEXT);
      }
      other => panic!("unexpected agent {other}"),
    }
  }
  let mut text = String::new();
  wait_for("keys delivered to the selected X11 window", || {
    while let Some(event) = conn.poll_for_event()? {
      if let Event::KeyPress(key) = event {
        assert_eq!(key.event, fixture);
        let keysym = conn.get_keyboard_mapping(key.detail, 1)?.reply()?.keysyms[0];
        text.push(char::from_u32(keysym).expect("ASCII fixture input"));
      }
    }
    Ok((text.len() >= expected.len()).then_some(()))
  })?;
  assert_eq!(text, expected);
  for entry in fs::read_dir(host.join("ctream/activity"))? {
    let bytes = fs::read(entry?.path())?;
    for payload in [FIRST_TEXT, SECOND_TEXT] {
      assert!(
        !bytes
          .windows(payload.len())
          .any(|window| window == payload.as_bytes()),
        "journal persisted typed text"
      );
    }
  }
  Ok(())
}
