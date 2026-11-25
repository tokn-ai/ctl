use super::*;
use ctream_client::transport::{Cancellation, Channel};
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
