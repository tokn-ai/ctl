use super::*;
use crate::{Client, transport::Channel};
use ctream_proto::{Target, Window};
use std::{
  io::Cursor,
  sync::{Arc, Mutex},
};

type Calls = Arc<Mutex<Vec<(Vec<String>, Vec<u8>)>>>;
struct Writer {
  calls: Calls,
  channel: usize,
}
impl Write for Writer {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    self.calls.lock().unwrap()[self.channel]
      .1
      .extend_from_slice(bytes);
    Ok(bytes.len())
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}
enum Reply {
  Stream(String),
  Reject(&'static str),
  Diagnostic(&'static str),
}
struct Diagnostic(&'static str);
impl Read for Diagnostic {
  fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
    Err(io::Error::other(self.0))
  }
}
struct Fixture {
  replies: Mutex<std::collections::VecDeque<Reply>>,
  calls: Calls,
}
impl Transport for Fixture {
  type Reader = Box<dyn Read + Send>;
  type Writer = Writer;
  fn open(&self, args: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    let mut calls = self.calls.lock().unwrap();
    let channel = calls.len();
    calls.push((args.to_vec(), Vec::new()));
    let reply = self
      .replies
      .lock()
      .unwrap()
      .pop_front()
      .expect("unexpected retry");
    let reader: Self::Reader = match reply {
      Reply::Reject(error) => return Err(io::Error::other(error)),
      Reply::Stream(output) => Box::new(Cursor::new(output.into_bytes())),
      Reply::Diagnostic(error) => Box::new(Diagnostic(error)),
    };
    Ok(Channel {
      reader,
      writer: Writer {
        calls: self.calls.clone(),
        channel,
      },
      cancellation: Cancellation::default(),
    })
  }
}
fn fixture(replies: impl IntoIterator<Item = Reply>) -> (SemanticSession<Fixture>, Calls) {
  let calls = Calls::default();
  let client = Client::new(Fixture {
    replies: Mutex::new(replies.into_iter().collect()),
    calls: calls.clone(),
  });
  let window = Window {
    id: "fixture".into(),
    title: "Controls".into(),
    app_id: "app".into(),
  };
  (
    SemanticSession::new(client.session(Target::parse("wayland-2").unwrap(), Some(window))),
    calls,
  )
}
fn ok(result: Value) -> Value {
  json!({"ok":true,"result":result})
}
fn rejection(code: &str) -> Value {
  json!({"ok":false,"code":code,"error":"turn rejected","owner_id":"other-participant"})
}
fn stream(values: &[Value]) -> Reply {
  Reply::Stream(values.iter().map(|v| format!("{v}\n")).collect())
}
fn text() -> Action {
  Action::SetText {
    path: "0/1".into(),
    text: "draft".into(),
  }
}
fn invoke() -> Action {
  Action::Invoke {
    path: "0/2".into(),
    index: 0,
    name: "activate".into(),
  }
}
fn requests(calls: &Calls) -> Vec<Value> {
  let calls = calls.lock().unwrap();
  String::from_utf8_lossy(&calls[0].1)
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

#[test]
fn current_actions_use_one_authenticated_source_and_expired_turns_stay_fenced() {
  let (mut controller, calls) = fixture([stream(&[
    ok(json!({"semantics":true})),
    rejection("busy"),
    ok(json!({"turn_id":"first"})),
    ok(Value::Null),
    rejection("stale_turn"),
    rejection("stale_turn"),
    ok(Value::Null),
  ])]);
  controller.connect("viewer").unwrap();
  assert!(controller.acquire(0).is_err());
  let busy = controller.acquire(MAX_TURN_MS).unwrap_err();
  assert!(busy.to_string().contains("other-participant"));
  controller.acquire(MAX_TURN_MS).unwrap();
  assert!(controller.apply(text()).unwrap().applied);
  assert!(is_stale(&controller.apply(invoke()).unwrap_err()));
  assert_eq!(controller.state(), SemanticTurn::Expired);
  assert!(is_stale(&controller.apply(invoke()).unwrap_err())); // local, no write
  controller.release().unwrap();
  assert_eq!(controller.state(), SemanticTurn::Available);
  assert!(controller.apply(invoke()).unwrap().applied);
  assert_eq!(
    calls.lock().unwrap()[0].0,
    ["1.1.18", "turn-session", "wayland-2", "viewer", "fixture"]
  );
  assert_eq!(
    requests(&calls),
    [
      json!({"operation":"capabilities"}),
      json!({"operation":"acquire_turn","ttl_ms":MAX_TURN_MS}),
      json!({"operation":"acquire_turn","ttl_ms":MAX_TURN_MS}),
      json!({"operation":"set_text","path":"0/1","text":"draft","turn_id":"first"}),
      json!({"operation":"invoke","path":"0/2","index":0,"name":"activate","turn_id":"first"}),
      json!({"operation":"release_turn","turn_id":"first"}),
      json!({"operation":"invoke","path":"0/2","index":0,"name":"activate"}),
    ]
  );
}

#[test]
fn a_late_reply_after_expiry_cannot_enable_an_ordinary_action() {
  let (mut controller, calls) = fixture([stream(&[
    ok(json!({"semantics":true})),
    ok(json!({"turn_id":"expired"})),
    ok(json!({"turn_id":"fresh"})),
    ok(Value::Null),
  ])]);
  controller.connect("observer").unwrap();
  controller.acquire(1000).unwrap();
  controller.lease.as_mut().unwrap().deadline = Instant::now();
  assert!(is_stale(&controller.apply(text()).unwrap_err()));
  assert_eq!(requests(&calls).len(), 2);
  controller.acquire(1000).unwrap();
  controller.apply(text()).unwrap();
  assert_eq!(requests(&calls).last().unwrap()["turn_id"], "fresh");
}

#[test]
fn only_an_explicit_version_rejection_allows_legacy_input() {
  let (mut current, calls) = fixture([Reply::Reject("permission denied")]);
  assert!(current.connect("viewer").is_err());
  assert!(current.apply(text()).is_err());
  assert_eq!(calls.lock().unwrap().len(), 1);

  let (mut legacy, calls) = fixture([
    Reply::Diagnostic(
      "agent channel exited with exit status: 1\nctream-agent: usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]",
    ),
    Reply::Stream(r#"{"applied":true,"error":null}"#.into()),
  ]);
  legacy.connect("viewer").unwrap();
  assert_eq!(legacy.state(), SemanticTurn::Legacy);
  assert!(legacy.acquire(1000).is_err());
  assert!(legacy.apply(text()).unwrap().applied);
  assert_eq!(requests(&calls), [json!({"operation":"capabilities"})]);
  let calls = calls.lock().unwrap();
  assert_eq!(calls[1].0[1], "action");
  assert_eq!(
    serde_json::from_slice::<Value>(&calls[1].1).unwrap(),
    serde_json::to_value(text()).unwrap()
  );
}

#[test]
fn uncertain_mutations_close_the_control_channel_without_retrying() {
  let (mut controller, calls) = fixture([stream(&[ok(json!({"semantics":true}))])]);
  controller.connect("viewer").unwrap();
  assert!(controller.apply(invoke()).is_err());
  assert_eq!(controller.state(), SemanticTurn::Disconnected);
  assert!(controller.apply(invoke()).is_err());
  assert!(controller.connect("viewer").is_err());
  assert_eq!(calls.lock().unwrap().len(), 1);
  assert_eq!(requests(&calls).len(), 2);
}
