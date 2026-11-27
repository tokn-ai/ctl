use super::*;
use crate::transport::{Cancellation, Channel};
use serde_json::{Value, json};
use std::io::{Cursor, Sink};

#[derive(Default)]
struct Agent {
  expected: &'static [&'static str],
  reply: &'static [u8],
}

impl Transport for Agent {
  type Reader = Cursor<&'static [u8]>;
  type Writer = Sink;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    assert_eq!(&arguments[1..], self.expected);
    Ok(Channel {
      reader: Cursor::new(self.reply),
      writer: io::sink(),
      cancellation: Cancellation::default(),
    })
  }
}

#[test]
fn window_screenshot_returns_decodable_rgb_pixels() {
  let client = Client::new(Agent {
    expected: &["capture-window", "windows", "win-test"],
    reply: b"P6\n2 1\n255\nabcdef",
  });
  let mut output = Vec::new();
  run(
    &client,
    Target::WindowsDesktop,
    Some("win-test".into()),
    br#"{"operation":"screenshot"}"#.as_slice(),
    &mut output,
  )
  .unwrap();
  let image: Value = serde_json::from_slice(&output).unwrap();
  assert_eq!(image["width"], 2);
  assert_eq!(image["height"], 1);
  assert_eq!(image["mime_type"], "image/png");
  let png = STANDARD.decode(image["data"].as_str().unwrap()).unwrap();
  let mut reader = png::Decoder::new(Cursor::new(png)).read_info().unwrap();
  let mut pixels = [0; 6];
  let frame = reader.next_frame(&mut pixels).unwrap();
  assert_eq!((frame.width, frame.height), (2, 1));
  assert_eq!(&pixels, b"abcdef");
}

#[test]
fn capabilities_and_control_respect_window_scope() {
  let client = Client::new(Agent::default());
  let mut output = Vec::new();
  let target = Target::Wayland(1);
  run(
    &client,
    target,
    Some("window-1".into()),
    br#"{"operation":"capabilities"}"#.as_slice(),
    &mut output,
  )
  .unwrap();
  assert_eq!(
    serde_json::from_slice::<Value>(&output).unwrap(),
    json!({"capture": true, "input": false, "semantics": true})
  );
  let error = run(
    &client,
    target,
    Some("window-1".into()),
    br#"{"operation":"type_text","text":"abc"}"#.as_slice(),
    io::sink(),
  )
  .unwrap_err();
  assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn successful_input_stays_on_the_selected_window() {
  let client = Client::new(Agent {
    expected: &["input-window", "windows", "win-test"],
    reply: b"ready\n",
  });
  let mut output = Vec::new();
  run(
    &client,
    Target::WindowsDesktop,
    Some("win-test".into()),
    br#"{"operation":"click","point":{"x":1,"y":0},"size":[2,1],"button":"left"}"#.as_slice(),
    &mut output,
  )
  .unwrap();
  assert_eq!(output, b"null\n");
}

#[test]
fn malformed_or_oversized_requests_never_send_input() {
  let client = Client::new(Agent::default());
  for request in [
    br#"{"operation":"press_keys","keys":["Tab"],"target":"macos"}"#.as_slice(),
    br#"{"operation":"screenshot"} {}"#.as_slice(),
    &vec![b' '; REQUEST_LIMIT + 1],
  ] {
    let error = run(&client, Target::X11(1), None, request, io::sink()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
  }
  for (target, window) in [
    (Target::MacosDesktop, None),
    (Target::MacosDesktop, Some("other-window".into())),
    (Target::Wayland(1), Some("bad id".into())),
  ] {
    assert!(
      run(
        &client,
        target,
        window,
        br#"{"operation":"capabilities"}"#.as_slice(),
        io::sink()
      )
      .is_err()
    );
  }
}

#[test]
fn semantic_reply_reports_application_failure() {
  let client = Client::new(Agent {
    expected: &["action", "wayland-1", "window-1"],
    reply: br#"{"applied":false,"error":"element disappeared"}"#,
  });
  let mut output = Vec::new();
  let error = run(
    &client,
    Target::Wayland(1),
    Some("window-1".into()),
    br#"{"operation":"invoke","path":"/1","index":0,"name":"click"}"#.as_slice(),
    &mut output,
  )
  .unwrap_err();
  assert_eq!(error.to_string(), "element disappeared");
  assert!(output.is_empty());
}

#[test]
fn session_flushes_one_reply_per_line_and_recovers_after_bad_requests() {
  let client = Client::new(Agent::default());
  let mut output = Vec::new();
  let mut records = vec![b' '; REQUEST_LIMIT - 2];
  records.extend_from_slice(b"{}\ninvalid\n{\"operation\":\"capabilities\"}\n");
  run_session(&client, Target::X11(1), None, &records[..], &mut output).unwrap();
  let replies: Vec<Value> = serde_json::Deserializer::from_slice(&output)
    .into_iter()
    .collect::<Result<_, _>>()
    .unwrap();
  assert_eq!(replies.len(), 3);
  assert_eq!(replies[0]["ok"], false);
  assert!(replies[1]["error"].is_string());
  assert_eq!(
    replies[2],
    json!({"ok": true, "result": {"capture": true, "input": true, "semantics": false}})
  );
}

#[test]
fn session_rejects_an_oversized_or_truncated_record_before_dispatch() {
  let client = Client::new(Agent::default());
  let mut oversized = vec![b' '; REQUEST_LIMIT + 1];
  oversized.extend_from_slice(b"\n{\"operation\":\"capabilities\"}\n");
  for data in [&oversized[..], br#"{"operation":"capabilities"}"#] {
    let mut output = Vec::new();
    assert!(run_session(&client, Target::X11(1), None, data, &mut output).is_err());
    assert!(output.is_empty());
  }
}

#[test]
fn attribution_can_gate_each_mutation_before_opening_any_channel() {
  let requests = [
    json!({"operation":"move","point":{"x":1,"y":2},"size":[3,4]}),
    json!({"operation":"click","point":{"x":1,"y":2},"size":[3,4],"button":"left"}),
    json!({"operation":"drag","from":{"x":0,"y":1},"to":{"x":2,"y":1},"size":[3,4],"button":"left"}),
    json!({"operation":"scroll","point":{"x":0,"y":1},"size":[3,4],"direction":"up","steps":1}),
    json!({"operation":"press_keys","keys":["Return"]}),
    json!({"operation":"type_text","text":"value"}),
    json!({"operation":"invoke","path":"/1","index":0,"name":"click"}),
    json!({"operation":"set_text","path":"/1","text":"value"}),
  ];
  let mut recorded = Vec::new();
  let mut output = Vec::new();
  let input = requests
    .iter()
    .map(|request| format!("{request}\n"))
    .collect::<String>();
  run_attributed_session(
    &Client::new(Agent::default()),
    Target::Windows(1),
    None,
    input.as_bytes(),
    &mut output,
    |name, _| {
      recorded.push(name);
      Err(io::Error::other("record unavailable"))
    },
  )
  .unwrap();
  assert_eq!(
    recorded,
    requests
      .iter()
      .map(|request| request["operation"].as_str().unwrap())
      .collect::<Vec<_>>()
  );
  let replies: Vec<Value> = serde_json::Deserializer::from_slice(&output)
    .into_iter()
    .collect::<Result<_, _>>()
    .unwrap();
  assert_eq!(
    replies,
    vec![json!({"ok":false,"error":"record unavailable"}); requests.len()]
  );
}

#[test]
fn observation_and_discovery_do_not_call_the_recorder() {
  for (operation, expected, reply) in [
    ("capabilities", &[][..], &b""[..]),
    (
      "elements",
      &["elements", "wayland-1", "window-1"][..],
      &b"[]"[..],
    ),
    (
      "screenshot",
      &["capture-window", "wayland-1", "window-1"][..],
      &b"P6\n1 1\n255\nabc"[..],
    ),
  ] {
    let client = Client::new(Agent { expected, reply });
    let session = select(&client, Target::Wayland(1), Some("window-1".into()));
    // The same typed dispatcher serves both persistent and one-shot requests.
    // One-shot capture can consume the fixture's single, finite frame.
    execute(
      &mut Execution::Once(&session),
      serde_json::from_value(json!({"operation":operation})).unwrap(),
      &mut |name, _| panic!("unexpected recorder call for {name:?}"),
    )
    .unwrap();
  }
}

#[test]
fn recorder_cannot_skip_replay_or_hide_a_failed_dispatch() {
  let mut count = 0;
  mutation(
    &mut |_, apply| {
      apply()?;
      assert!(apply().is_err());
      Ok(())
    },
    LiveAction::Click { point: [0.5, 0.5] },
    || {
      count += 1;
      Ok(())
    },
  )
  .unwrap();
  assert_eq!(count, 1);
  assert!(
    mutation(
      &mut |_, _| Ok(()),
      LiveAction::Click { point: [0.5, 0.5] },
      || panic!("skipped operation ran")
    )
    .is_err()
  );
  assert!(
    mutation(
      &mut |_, apply| {
        let _ = apply();
        Ok(())
      },
      LiveAction::Click { point: [0.5, 0.5] },
      || Err(io::ErrorKind::PermissionDenied.into())
    )
    .is_err()
  );
}

#[test]
fn host_session_dispatches_typed_requests_and_negotiates_action_receipts() {
  use ctream_proto::live::{ActionFailure, TurnError};

  struct Desktop;
  impl SessionAuthority for Desktop {
    fn acquire(&mut self, _: u64) -> io::Result<String> {
      unreachable!()
    }
    fn release(&mut self, _: &str) -> io::Result<()> {
      unreachable!()
    }
    fn dispatch(&mut self, _: Option<&str>, action: Mutation) -> io::Result<String> {
      match action {
        Mutation::PressKeys { keys } if keys == ["a"] => Ok("applied".to_owned()),
        Mutation::TypeText { text } if text == "private" => Err(io::Error::other(ActionFailure {
          action_id: "refused".to_owned(),
          source: io::Error::new(
            io::ErrorKind::WouldBlock,
            TurnError {
              code: "busy".to_owned(),
              owner_id: Some("alice".to_owned()),
            },
          ),
        })),
        _ => panic!("unexpected request"),
      }
    }
  }

  let input = b"{\"operation\":\"press_keys\",\"keys\":[\"a\"]}\n{\"operation\":\"type_text\",\"text\":\"private\"}\n";
  for protocol in [
    SessionProtocol::Attributed,
    SessionProtocol::Turns,
    SessionProtocol::Daemon,
  ] {
    let mut output = Vec::new();
    run_host_session(
      &Client::new(Agent::default()),
      Target::X11(1),
      None,
      input.as_slice(),
      &mut output,
      protocol,
      &mut Desktop,
    )
    .unwrap();
    let replies: Vec<Value> = serde_json::Deserializer::from_slice(&output)
      .into_iter()
      .collect::<Result<_, _>>()
      .unwrap();
    assert_eq!(replies[0]["ok"], true);
    assert!(replies[0]["result"].is_null());
    assert_eq!(replies[1]["ok"], false);
    assert!(replies[1]["error"].is_string());
    let admission = protocol != SessionProtocol::Attributed;
    assert_eq!(replies[1]["code"].as_str(), admission.then_some("busy"));
    assert_eq!(
      replies[1]["owner_id"].as_str(),
      admission.then_some("alice")
    );
    let receipts = protocol == SessionProtocol::Daemon;
    assert_eq!(
      replies[0].get("action_id").and_then(Value::as_str),
      receipts.then_some("applied")
    );
    assert_eq!(
      replies[1].get("action_id").and_then(Value::as_str),
      receipts.then_some("refused")
    );
    assert!(!String::from_utf8(output).unwrap().contains("private"));
  }
}
