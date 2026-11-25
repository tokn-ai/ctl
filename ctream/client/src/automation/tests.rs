use super::*;
use crate::{
  Client,
  transport::{Cancellation, Channel},
};
use ctream_proto::Window;
use std::{
  collections::VecDeque,
  io::Cursor,
  sync::{Arc, Mutex, mpsc},
  time::Duration,
};

#[derive(Default)]
struct Record {
  writes: Vec<u8>,
  events: Vec<String>,
  fail_flush: Option<usize>,
  flushes: usize,
}

struct Mock {
  responses: Mutex<VecDeque<&'static str>>,
  record: Arc<Mutex<Record>>,
}

struct Writer {
  _open: mpsc::Sender<()>,
  record: Arc<Mutex<Record>>,
}

impl Write for Writer {
  fn write(&mut self, data: &[u8]) -> io::Result<usize> {
    self.record.lock().unwrap().writes.extend_from_slice(data);
    Ok(data.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    let mut record = self.record.lock().unwrap();
    record.flushes += 1;
    if Some(record.flushes) == record.fail_flush {
      return Err(io::ErrorKind::BrokenPipe.into());
    }
    Ok(())
  }
}

// A remote operation exits only after its input closes. This also detects
// teardown that tries to join the frame reader before closing its writer.
struct Eof {
  closed: mpsc::Receiver<()>,
  operation: String,
  record: Arc<Mutex<Record>>,
}

impl Read for Eof {
  fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
    if matches!(
      self.closed.recv_timeout(Duration::from_secs(2)),
      Err(mpsc::RecvTimeoutError::Timeout)
    ) {
      return Err(io::ErrorKind::TimedOut.into());
    }
    self
      .record
      .lock()
      .unwrap()
      .events
      .push(format!("done {}", self.operation));
    Ok(0)
  }
}

impl Transport for Mock {
  type Reader = io::Chain<Cursor<&'static [u8]>, Eof>;
  type Writer = Writer;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    let operation = &arguments[1];
    let record = Arc::clone(&self.record);
    record
      .lock()
      .unwrap()
      .events
      .push(format!("open {operation}"));
    let reply = self
      .responses
      .lock()
      .unwrap()
      .pop_front()
      .expect("unexpected channel");
    let (open, closed) = mpsc::channel();
    Ok(Channel {
      writer: Writer {
        _open: open,
        record: Arc::clone(&record),
      },
      reader: Cursor::new(reply.as_bytes()).chain(Eof {
        closed,
        operation: operation.clone(),
        record,
      }),
      cancellation: Cancellation::default(),
    })
  }
}

fn client(responses: &[&'static str]) -> (Client<Mock>, Arc<Mutex<Record>>) {
  let record = Arc::new(Mutex::new(Record::default()));
  (
    Client::new(Mock {
      responses: Mutex::new(responses.iter().copied().collect()),
      record: Arc::clone(&record),
    }),
    record,
  )
}

#[test]
fn gestures_share_input_and_use_screenshot_coordinates() {
  let (client, record) = client(&["1920 1080\n"]);
  let mut controls = client.session(Target::X11(0), None).controls().unwrap();
  let size = [640, 360];
  let from = Point { x: 0, y: 0 };
  let to = Point { x: 639, y: 359 };
  assert!(
    controls
      .drag(from, Point { x: 640, y: 0 }, size, MouseButton::Right)
      .is_err()
  );
  assert!(
    controls
      .scroll(from, size, ScrollDirection::Down, 0)
      .is_err()
  );
  controls.click(to, size, MouseButton::Left).unwrap();
  controls.drag(from, to, size, MouseButton::Right).unwrap();
  controls
    .scroll(from, size, ScrollDirection::Left, 2)
    .unwrap();
  controls.finish().unwrap();
  let record = record.lock().unwrap();
  assert_eq!(record.events, ["open input", "done input"]);
  assert_eq!(
    record.writes,
    concat!(
      "move 1919 1079\nbutton 1 1\nbutton 1 0\n",
      "move 0 0\nbutton 3 1\nmove 1919 1079\nbutton 3 0\n",
      "move 0 0\nbutton 6 1\nbutton 6 0\nbutton 6 1\nbutton 6 0\n"
    )
    .as_bytes()
  );
}

#[test]
fn chord_and_text_validate_before_writing_and_release_in_reverse_order() {
  let (client, record) = client(&["ready\n"]);
  let mut controls = client.session(Target::Windows(1), None).controls().unwrap();
  for chord in [
    vec!["Control_L".into(), "a\nreset".into()],
    vec!["a".into(), "a".into()],
    vec![],
  ] {
    assert!(controls.press_keys(&chord).is_err());
  }
  for text in ["ok🦀", "ok\0"] {
    assert!(controls.type_text(text).is_err());
  }
  assert!(record.lock().unwrap().writes.is_empty());
  controls.move_to(Point { x: 5, y: 3 }, [10, 8]).unwrap();
  controls
    .press_keys(&["Control_L".into(), "a".into()])
    .unwrap();
  controls.type_text("A!\t\r\n").unwrap();
  controls.finish().unwrap();
  assert_eq!(
    record.lock().unwrap().writes,
    concat!(
      "move 5 3 10 8\n",
      "key Control_L 1\nkey a 1\nkey a 0\nkey Control_L 0\n",
      "key Shift_L 1\nkey a 1\nkey a 0\nkey Shift_L 0\n",
      "key Shift_L 1\nkey 1 1\nkey 1 0\nkey Shift_L 0\n",
      "key Tab 1\nkey Tab 0\nkey Return 1\nkey Return 0\n"
    )
    .as_bytes()
  );
  for byte in b' '..=b'~' {
    assert!(!keyboard::us_key(byte).0.is_empty());
  }
}

#[test]
fn write_failure_after_button_down_closes_controls() {
  let (client, record) = client(&["ready\n"]);
  let mut controls = client.session(Target::Windows(1), None).controls().unwrap();
  record.lock().unwrap().fail_flush = Some(2);
  let point = Point { x: 1, y: 2 };
  assert_eq!(
    controls
      .click(point, [3, 3], MouseButton::Middle)
      .unwrap_err()
      .kind(),
    io::ErrorKind::BrokenPipe
  );
  assert_eq!(
    controls.move_to(point, [3, 3]).unwrap_err().kind(),
    io::ErrorKind::NotConnected
  );
}

#[test]
fn macos_screenshots_and_input_release_capture_before_next_operation() {
  let (client, record) = client(&[
    "P6\n1 1\n255\nabc",
    "{\"driver\":\"macos.sck\",\"format\":{\"type\":\"ppm\"}}\nP6\n1 1\n255\nrgb",
    "ready\n",
    "P6\n1 1\n255\nxyz",
  ]);
  let window = Window {
    id: "macos-7-9".into(),
    ..Window::default()
  };
  let session = client.session(Target::MacosDesktop, Some(window));
  assert_eq!(session.screenshot().unwrap().pixels, b"abc");
  {
    let mut controls = session.controls().unwrap();
    controls.type_text("x").unwrap();
  }
  assert_eq!(session.screenshot().unwrap().pixels, b"xyz");
  assert_eq!(
    record.lock().unwrap().events,
    [
      "open capture-window",
      "done capture-window",
      "open capture-with-driver",
      "open input-window",
      "done input-window",
      "done capture-with-driver",
      "open capture-window",
      "done capture-window",
    ]
  );
}

#[test]
fn bad_macos_input_handshake_still_finishes_capture() {
  let (client, record) = client(&[
    "{\"driver\":\"macos.sck\",\"format\":{\"type\":\"ppm\"}}\n",
    "denied\n",
  ]);
  let window = Window {
    id: "macos-7-9".into(),
    ..Window::default()
  };
  assert!(
    client
      .session(Target::MacosDesktop, Some(window))
      .controls()
      .is_err()
  );
  assert_eq!(
    record.lock().unwrap().events,
    [
      "open capture-with-driver",
      "open input-window",
      "done capture-with-driver",
    ]
  );
}
