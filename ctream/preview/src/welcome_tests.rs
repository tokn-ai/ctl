use super::*;
use ctream_client::transport::{Cancellation, Channel};
use std::{
  collections::VecDeque,
  io::{Read, Write},
  sync::{Condvar, Mutex},
  thread,
};

const DEADLINE: Duration = Duration::from_secs(4);
const DISPLAYS: &str = concat!(
  r#"[{"display":{"windows":{"number":1,"name":"test one","bounds":[0,0,4,2]}},"class":null,"selectable":true},"#,
  r#"{"display":{"windows":{"number":2,"name":"test two","bounds":[4,0,8,2]}},"class":null,"selectable":true}]"#,
);
const DRIVERS: &str = concat!(
  r#"[{"id":"windows.dxgi","name":"DXGI","scopes":["display"]},"#,
  r#"{"id":"windows.wgc","name":"WGC","scopes":["window"]}]"#,
);

#[derive(Default)]
struct Wire {
  incoming: VecDeque<u8>,
  eof: bool,
  waiting: bool,
  cancelled: bool,
  writer_closed: bool,
  sent: Vec<u8>,
}

struct Record {
  arguments: Vec<String>,
  wire: Mutex<Wire>,
  wake: Condvar,
}

impl Record {
  fn operation(&self) -> &str {
    &self.arguments[1]
  }
  fn target(&self) -> Option<&str> {
    self.arguments.get(2).map(String::as_str)
  }
  fn closed(&self) -> bool {
    self.wire.lock().unwrap().writer_closed
  }
  fn cancelled(&self) -> bool {
    self.wire.lock().unwrap().cancelled
  }
  fn sent(&self) -> String {
    String::from_utf8(self.wire.lock().unwrap().sent.clone()).unwrap()
  }
  fn push_frame(&self) {
    let mut wire = self.wire.lock().unwrap();
    assert!(
      !wire.eof,
      "cannot deliver another frame after capture closes"
    );
    wire.incoming.extend(b"P6\n2 1\n255\nghijkl");
    self.wake.notify_all();
  }
}

struct Reader(Arc<Record>);

impl Read for Reader {
  fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
    if output.is_empty() {
      return Ok(0);
    }
    let mut wire = self.0.wire.lock().unwrap();
    if wire.incoming.is_empty() && !wire.eof {
      wire.waiting = true;
      let (updated, timeout) = self
        .0
        .wake
        .wait_timeout_while(wire, DEADLINE, |wire| wire.incoming.is_empty() && !wire.eof)
        .unwrap();
      wire = updated;
      if timeout.timed_out() && wire.incoming.is_empty() && !wire.eof {
        return Err(io::Error::new(
          io::ErrorKind::TimedOut,
          "mock channel was not closed",
        ));
      }
    }
    let count = output.len().min(wire.incoming.len());
    for byte in &mut output[..count] {
      *byte = wire.incoming.pop_front().unwrap();
    }
    Ok(count)
  }
}

struct Writer(Arc<Record>);

impl Write for Writer {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    self.0.wire.lock().unwrap().sent.extend_from_slice(bytes);
    Ok(bytes.len())
  }
  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

impl Drop for Writer {
  fn drop(&mut self) {
    let mut wire = self.0.wire.lock().unwrap();
    wire.writer_closed = true;
    wire.eof = true;
    self.0.wake.notify_all();
  }
}

#[derive(Default)]
struct Gate {
  released: Mutex<bool>,
  wake: Condvar,
}

impl Gate {
  fn release(&self) {
    *self.released.lock().unwrap() = true;
    self.wake.notify_all();
  }

  fn wait(&self) -> io::Result<()> {
    let (released, _) = self
      .wake
      .wait_timeout_while(self.released.lock().unwrap(), DEADLINE, |released| {
        !*released
      })
      .unwrap();
    if *released {
      Ok(())
    } else {
      Err(io::Error::other("mock open was not released"))
    }
  }
}

#[derive(Default)]
struct Config {
  legacy: bool,
  capture_failure: bool,
  wait_input: bool,
  gates: Vec<(String, String, Arc<Gate>)>,
}

#[derive(Default)]
struct Observed {
  records: Mutex<Vec<Arc<Record>>>,
  config: Mutex<Config>,
}

#[derive(Clone, Default)]
struct Mock(Arc<Observed>);

impl Mock {
  fn records(&self, operation: &str) -> Vec<Arc<Record>> {
    self
      .0
      .records
      .lock()
      .unwrap()
      .iter()
      .filter(|record| record.operation() == operation)
      .cloned()
      .collect()
  }

  fn captures(&self) -> Vec<Arc<Record>> {
    self
      .0
      .records
      .lock()
      .unwrap()
      .iter()
      .filter(|record| {
        matches!(
          record.operation(),
          "capture" | "capture-window" | "capture-with-driver"
        )
      })
      .cloned()
      .collect()
  }

  fn inputs(&self) -> Vec<Arc<Record>> {
    self
      .0
      .records
      .lock()
      .unwrap()
      .iter()
      .filter(|record| matches!(record.operation(), "input" | "input-window"))
      .cloned()
      .collect()
  }

  fn delay(&self, operation: &str, target: &str) -> Arc<Gate> {
    let gate = Arc::new(Gate::default());
    self
      .0
      .config
      .lock()
      .unwrap()
      .gates
      .push((operation.into(), target.into(), Arc::clone(&gate)));
    gate
  }
}

impl Transport for Mock {
  type Reader = Reader;
  type Writer = Writer;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Reader, Writer>> {
    let operation = arguments[1].as_str();
    let target = arguments.get(2).map(String::as_str).unwrap_or("");
    let mut config = self.0.config.lock().unwrap();
    let (mut incoming, eof) = match operation {
      "discover" => (DISPLAYS.as_bytes().to_vec(), true),
      "windows" => (
        format!(
          r#"[{{"id":"window-{}","title":"Owned fixture","app_id":"fixture"}}]"#,
          target.trim_start_matches("windows-")
        )
        .into_bytes(),
        true,
      ),
      "capture-drivers" => (
        if config.legacy {
          Vec::new()
        } else {
          DRIVERS.as_bytes().to_vec()
        },
        true,
      ),
      "input" | "input-window" => (
        if config.wait_input {
          Vec::new()
        } else {
          b"ready\n".to_vec()
        },
        false,
      ),
      "capture-with-driver" if config.capture_failure => (b"backend unavailable\n".to_vec(), true),
      "capture-with-driver" => {
        let driver = if arguments[3] == "auto" {
          if arguments.len() == 5 {
            "windows.wgc"
          } else {
            "windows.dxgi"
          }
        } else {
          &arguments[3]
        };
        (
          format!("{{\"driver\":\"{driver}\",\"format\":{{\"type\":\"ppm\"}}}}\n").into_bytes(),
          false,
        )
      }
      "capture" | "capture-window" => (Vec::new(), false),
      _ => return Err(io::Error::other("unexpected mock operation")),
    };
    if matches!(
      operation,
      "capture" | "capture-window" | "capture-with-driver"
    ) && !eof
    {
      incoming.extend_from_slice(if target == "windows-2" {
        b"P6\n3 1\n255\nabcdefghi"
      } else {
        b"P6\n2 1\n255\nabcdef"
      });
    }
    let gate = config
      .gates
      .iter()
      .position(|(op, display, _)| op == operation && display == target)
      .map(|index| config.gates.remove(index).2);
    drop(config);
    let record = Arc::new(Record {
      arguments: arguments.to_vec(),
      wire: Mutex::new(Wire {
        incoming: incoming.into(),
        eof,
        ..Default::default()
      }),
      wake: Condvar::new(),
    });
    self.0.records.lock().unwrap().push(Arc::clone(&record));
    if let Some(gate) = gate {
      gate.wait()?;
    }
    let cancelled = Arc::clone(&record);
    Ok(Channel {
      reader: Reader(Arc::clone(&record)),
      writer: Writer(record),
      cancellation: Cancellation::new(move || {
        let mut wire = cancelled.wire.lock().unwrap();
        wire.cancelled = true;
        wire.eof = true;
        cancelled.wake.notify_all();
      }),
    })
  }
}

fn raw(events: Vec<egui::Event>) -> egui::RawInput {
  egui::RawInput {
    screen_rect: Some(egui::Rect::from_min_size(
      egui::Pos2::ZERO,
      egui::vec2(860.0, 620.0),
    )),
    focused: true,
    events,
    ..Default::default()
  }
}

fn poll_until(
  viewer: &mut Viewer<Mock>,
  ctx: &egui::Context,
  done: impl Fn(&Viewer<Mock>) -> bool,
) {
  let deadline = Instant::now() + DEADLINE;
  loop {
    let _ = ctx.run(raw(Vec::new()), |ctx| viewer.poll(ctx));
    if done(viewer) {
      return;
    }
    assert!(
      Instant::now() < deadline,
      "welcome condition timed out; discovery={:?}, preview={:?}, selection={:?}",
      viewer.discovery_error,
      viewer.preview_error,
      viewer.selection_error()
    );
    thread::sleep(Duration::from_millis(2));
  }
}

fn ready(viewer: &Viewer<Mock>) -> bool {
  viewer.preview.as_ref().is_some_and(Preview::ready)
}

fn create(mock: &Mock, ctx: &egui::Context, window: bool, driver: &str) -> Viewer<Mock> {
  Viewer::new(
    mock.clone(),
    Some(Target::Windows(1)),
    window,
    window.then(|| "window-1".into()),
    driver.into(),
    ctx,
  )
}

fn render(
  viewer: &mut Viewer<Mock>,
  ctx: &egui::Context,
  events: Vec<egui::Event>,
) -> egui::FullOutput {
  ctx.run(raw(events), |ctx| {
    viewer.poll(ctx);
    if viewer.viewing {
      viewer.show_viewer(ctx);
    } else {
      viewer.show_setup(ctx);
    }
  })
}

fn image_center(viewer: &mut Viewer<Mock>, ctx: &egui::Context) -> egui::Pos2 {
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  render(viewer, ctx, Vec::new())
    .shapes
    .iter()
    .find_map(|shape| match &shape.shape {
      egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == texture => {
        Some(mesh.calc_bounds().center())
      }
      _ => None,
    })
    .expect("preview image must be drawn")
}

fn interactions(at: egui::Pos2, hold_key: bool) -> Vec<egui::Event> {
  let mut events = vec![
    egui::Event::PointerMoved(at),
    egui::Event::PointerButton {
      pos: at,
      button: egui::PointerButton::Primary,
      pressed: true,
      modifiers: egui::Modifiers::NONE,
    },
    egui::Event::PointerButton {
      pos: at,
      button: egui::PointerButton::Primary,
      pressed: false,
      modifiers: egui::Modifiers::NONE,
    },
    egui::Event::MouseWheel {
      unit: egui::MouseWheelUnit::Line,
      delta: egui::vec2(0.0, 1.0),
      modifiers: egui::Modifiers::NONE,
    },
    egui::Event::Key {
      key: egui::Key::F8,
      physical_key: Some(egui::Key::F8),
      pressed: true,
      repeat: false,
      modifiers: egui::Modifiers::NONE,
    },
  ];
  if !hold_key {
    events.push(egui::Event::Key {
      key: egui::Key::F8,
      physical_key: Some(egui::Key::F8),
      pressed: false,
      repeat: false,
      modifiers: egui::Modifiers::NONE,
    });
  }
  events
}

#[test]
fn welcome_interactions_stay_local_and_start_back_reuse_the_capture() {
  for window in [false, true] {
    let mock = Mock::default();
    let ctx = egui::Context::default();
    let mut viewer = create(&mock, &ctx, window, "auto");
    viewer.start(&ctx);
    assert!(!viewer.viewing, "Start must wait for a live frame");
    poll_until(&mut viewer, &ctx, ready);
    let stream = Arc::clone(&viewer.preview.as_ref().unwrap().stream);
    let capture = mock.captures().pop().unwrap();
    let at = image_center(&mut viewer, &ctx);
    render(&mut viewer, &ctx, interactions(at, false));
    assert!(
      mock.inputs().is_empty(),
      "welcome gestures must never open input"
    );
    assert!(!viewer.viewing);
    viewer.start(&ctx);
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().input.is_some()
    });
    assert!(Arc::ptr_eq(
      &stream,
      &viewer.preview.as_ref().unwrap().stream
    ));
    assert_eq!(mock.captures().len(), 1);
    let input = mock.inputs().pop().unwrap();
    assert_eq!(
      input.operation(),
      if window { "input-window" } else { "input" }
    );
    let at = image_center(&mut viewer, &ctx);
    render(&mut viewer, &ctx, interactions(at, true));
    assert!(input.sent().contains("button 1 1\n"));
    assert!(input.sent().contains("key F8 1\n"));
    viewer.back(&ctx);
    assert!(
      input.closed() && input.cancelled(),
      "Back must close the channel holding F8"
    );
    assert!(!capture.closed() && !capture.cancelled());
    assert!(ready(&viewer));
    let frames = viewer.preview.as_ref().unwrap().frames;
    capture.push_frame();
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().frames > frames
    });
    assert!(Arc::ptr_eq(
      &stream,
      &viewer.preview.as_ref().unwrap().stream
    ));
    let before = input.sent();
    let at = image_center(&mut viewer, &ctx);
    render(&mut viewer, &ctx, interactions(at, false));
    assert_eq!(mock.inputs().len(), 1);
    assert_eq!(input.sent(), before);
    viewer.start(&ctx);
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().input.is_some()
    });
    assert_eq!(mock.inputs().len(), 2);
    assert_eq!(mock.captures().len(), 1);
    drop(viewer);
    assert!(capture.closed() && capture.cancelled());
    assert!(
      mock
        .inputs()
        .iter()
        .all(|input| input.closed() && input.cancelled())
    );
  }
}

#[test]
fn switching_sources_cancels_capture_and_ignores_late_connections_and_queries() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let original = mock.captures().pop().unwrap();
  let capture_gate = mock.delay("capture-with-driver", "windows-2");
  let windows_gate = mock.delay("windows", "windows-2");
  viewer.selected = Some(Target::Windows(2));
  viewer.load_target(&ctx);
  assert!(original.closed() && original.cancelled());
  assert!(viewer.preview.is_none());
  poll_until(&mut viewer, &ctx, |_| {
    mock
      .captures()
      .iter()
      .any(|record| record.target() == Some("windows-2"))
  });
  let stale = mock
    .captures()
    .into_iter()
    .find(|record| record.target() == Some("windows-2"))
    .unwrap();
  viewer.selected = Some(Target::Windows(1));
  viewer.load_target(&ctx);
  poll_until(&mut viewer, &ctx, ready);
  let current = Arc::clone(&viewer.preview.as_ref().unwrap().stream);
  capture_gate.release();
  windows_gate.release();
  poll_until(&mut viewer, &ctx, |viewer| {
    stale.closed() && stale.cancelled() && viewer.window_query.is_none()
  });
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert_eq!(viewer.windows[0].id, "window-1");
  assert!(Arc::ptr_eq(
    &current,
    &viewer.preview.as_ref().unwrap().stream
  ));
  assert_eq!(
    viewer
      .preview
      .as_ref()
      .unwrap()
      .texture
      .as_ref()
      .unwrap()
      .size(),
    [2, 1]
  );
  assert!(viewer.preview_error.is_none());
  assert!(mock.inputs().is_empty());
}

#[test]
fn back_during_input_handshake_cancels_control_without_stopping_capture() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().wait_input = true;
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let capture = mock.captures().pop().unwrap();
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |_| {
    mock
      .inputs()
      .iter()
      .any(|input| input.wire.lock().unwrap().waiting)
  });
  let input = mock.inputs().pop().unwrap();
  viewer.back(&ctx);
  poll_until(&mut viewer, &ctx, |_| input.closed() && input.cancelled());
  assert!(!viewer.viewing && viewer.control.is_none());
  assert!(ready(&viewer));
  assert!(viewer.preview.as_ref().unwrap().input.is_none());
  assert!(viewer.preview.as_ref().unwrap().input_error.is_none());
  assert!(!capture.closed() && !capture.cancelled());
  assert_eq!(mock.captures().len(), 1);
  let frames = viewer.preview.as_ref().unwrap().frames;
  capture.push_frame();
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().frames > frames
  });
}

#[test]
fn explicit_failed_driver_never_retries_auto_or_legacy_capture() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().capture_failure = true;
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "windows.dxgi");
  poll_until(&mut viewer, &ctx, |viewer| viewer.preview_error.is_some());
  let captures = mock.captures();
  assert_eq!(captures.len(), 1);
  assert_eq!(captures[0].operation(), "capture-with-driver");
  assert_eq!(captures[0].arguments[3], "windows.dxgi");
  assert!(captures[0].closed() && captures[0].cancelled());
  viewer.start(&ctx);
  assert!(!viewer.viewing && mock.inputs().is_empty());
}

#[test]
fn unavailable_or_wrong_scope_explicit_driver_never_opens_capture() {
  for (driver, legacy) in [
    ("custom.missing", false),
    ("windows.wgc", false),
    ("windows.dxgi", true),
  ] {
    let mock = Mock::default();
    mock.0.config.lock().unwrap().legacy = legacy;
    let ctx = egui::Context::default();
    let mut viewer = create(&mock, &ctx, false, driver);
    poll_until(&mut viewer, &ctx, |viewer| viewer.catalog_ready);
    assert!(viewer.selection_error().is_some());
    assert!(viewer.connecting.is_none());
    assert!(mock.captures().is_empty());
    viewer.start(&ctx);
    assert!(!viewer.viewing && mock.inputs().is_empty());
  }
}

#[test]
fn legacy_auto_uses_unprefixed_capture_without_claiming_a_driver() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().legacy = true;
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  assert!(viewer.driver_error.is_some());
  assert!(viewer.preview.as_ref().unwrap().driver.is_none());
  assert_eq!(mock.records("capture").len(), 1);
  assert!(mock.records("capture-with-driver").is_empty());
  assert!(mock.inputs().is_empty());
}
