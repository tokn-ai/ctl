use super::wait_for;
use ctream_client::{Client, transport::Local};
use ctream_proto::{Target, WindowId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
  collections::HashSet,
  error::Error,
  fs,
  io::{self, BufRead, BufReader, Write},
  path::{Path, PathBuf},
  process::{Child, ChildStdout, Command, Stdio},
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
// The SSH-compatible launcher receives -T HOST COMMAND. Execute the same
// generated remote command in the display host's isolated state directory.
const TEST_SSH: &str = r#"
set -eu
test "$#" -eq 3 && test "$1" = -T && test "$2" = fixture-display || exit 2
export XDG_STATE_HOME="$CTREAM_TEST_HOST_STATE"
exec sh -c "$3"
"#;

struct State(PathBuf);

impl Drop for State {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

struct NamedClient {
  child: Child,
  output: BufReader<ChildStdout>,
}

impl NamedClient {
  fn open(
    caller: &Path,
    host: &Path,
    target: &str,
    window: &str,
    agent_id: Option<&str>,
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
        "session",
        target,
        "--window",
        window,
      ]);
    if let Some(agent_id) = agent_id {
      command.args(["--protocol", "1.0.3", "--agent-id", agent_id]);
    }
    let mut child = command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .spawn()?;
    let output = BufReader::new(child.stdout.take().unwrap());
    Ok(Self { child, output })
  }

  fn send(&mut self, request: Value) -> io::Result<()> {
    let input = self.child.stdin.as_mut().unwrap();
    serde_json::to_writer(&mut *input, &request)?;
    input.write_all(b"\n")?;
    input.flush()
  }

  fn receive(&mut self) -> io::Result<Value> {
    let mut line = String::new();
    self.output.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
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

#[test]
#[ignore = "requires the disposable X11 session and sibling CLI in docker/ctream"]
fn two_named_clients_attribute_physical_input_to_the_same_window() -> Result<(), Box<dyn Error>> {
  let state = State(std::env::temp_dir().join(format!("ctream-attribution-{}", Uuid::new_v4())));
  let host = state.0.join("display-host");
  let caller_a = state.0.join("caller-a");
  let caller_b = state.0.join("caller-b");
  fs::create_dir_all(&caller_a)?;
  fs::create_dir(&caller_b)?;
  let display = std::env::var("CTREAM_TEST_X11").unwrap_or_else(|_| ":98".into());
  let (conn, screen) = x11rb::connect(Some(&display))?;
  let screen = &conn.setup().roots[screen];
  let fixture = conn.generate_id()?;
  conn
    .create_window(
      COPY_DEPTH_FROM_PARENT,
      fixture,
      screen.root,
      20,
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
      b"Ctream named fixture",
    )?
    .check()?;
  conn.map_window(fixture)?.check()?;
  let target = Target::parse(&display)?;
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
  let mut a = NamedClient::open(&caller_a, &host, &display, &window, None)?;
  let mut b = NamedClient::open(&caller_b, &host, &display, &window, Some("writer-b"))?;

  // Both independently launched clients hold the same live window open.
  for client in [&mut a, &mut b] {
    client.send(json!({"operation": "screenshot"}))?;
    let reply = client.receive()?;
    assert_eq!(reply["result"]["mime_type"], "image/png", "{reply}");
  }
  assert!(
    activity(&host)?.is_empty(),
    "observations created mutation records"
  );
  // Submit both before waiting on either. The physical event order must agree
  // with the host ledger and each full text action must stay contiguous.
  a.send(json!({"operation":"type_text", "text": FIRST_TEXT}))?;
  b.send(json!({"operation":"type_text", "text": SECOND_TEXT}))?;
  for client in [&mut a, &mut b] {
    assert_eq!(client.receive()?, json!({"ok": true, "result": null}));
  }
  a.send(json!({"operation":"click", "point":{"x":220,"y":1}, "size":[220,100], "button":"left"}))?;
  assert_eq!(a.receive()?["ok"], false);
  a.close()?;
  b.close()?;

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
  let rows = activity(&host)?;
  assert_eq!(rows.len(), 3);
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
  for row in rows[1..].iter().rev() {
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
