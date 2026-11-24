use super::*;
use crate::view::WindowState;
use ctream_client::transport::{Cancellation, Channel};
use std::{
  cell::RefCell,
  collections::{HashMap, VecDeque},
  io::{Read, Write},
  rc::Rc,
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
const CATALOG: &str = concat!(
  r#"[{"id":"windows.dxgi","name":"DXGI","scopes":["display"],"systems":["windows"]},"#,
  r#"{"id":"windows.wgc","name":"WGC","scopes":["window"],"systems":["windows"]}]"#,
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
    self.push_frame_bytes(b"P6\n2 1\n255\nghijkl");
  }
  fn push_frame_bytes(&self, frame: &[u8]) {
    let mut wire = self.wire.lock().unwrap();
    assert!(
      !wire.eof,
      "cannot deliver another frame after capture closes"
    );
    wire.incoming.extend(frame);
    self.wake.notify_all();
  }

  fn finish(&self) {
    self.wire.lock().unwrap().eof = true;
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
  desktop_windows: bool,
  per_target_catalog_only: bool,
  catalog: Option<String>,
  displays: Option<String>,
  capture_failure: bool,
  first_window_closed: bool,
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
      "discover" => (
        config
          .displays
          .as_deref()
          .unwrap_or(DISPLAYS)
          .as_bytes()
          .to_vec(),
        true,
      ),
      "windows" if target == "windows" => (
        if config.desktop_windows {
          br#"[{"id":"window-desktop","title":"Desktop fixture","app_id":"fixture"}]"#.to_vec()
        } else {
          Vec::new()
        },
        true,
      ),
      "windows" if target == "macos" => (
        br#"[{"id":"macos-31-42","title":"Mac fixture","app_id":"fixture"}]"#.to_vec(),
        true,
      ),
      "windows" if target == "windows-1" && config.first_window_closed => (b"[]".to_vec(), true),
      "windows" => (
        format!(
          r#"[{{"id":"window-{}","title":"Owned fixture","app_id":"fixture"}}]"#,
          target.trim_start_matches("windows-")
        )
        .into_bytes(),
        true,
      ),
      "capture-drivers" => (
        if config.legacy || (target.is_empty() && config.per_target_catalog_only) {
          Vec::new()
        } else if target.is_empty() {
          config
            .catalog
            .as_deref()
            .unwrap_or(CATALOG)
            .as_bytes()
            .to_vec()
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
          if target == "macos" {
            "macos.screencapturekit"
          } else if arguments.len() == 5 {
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

fn run_headless(
  ctx: &egui::Context,
  input: egui::RawInput,
  draw: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
  let mut output = ctx.run_ui(input, draw);
  // These tests inspect geometry and viewport commands without a GPU renderer.
  output.textures_delta.clear();
  output
}

fn poll_until(
  viewer: &mut Viewer<Mock>,
  ctx: &egui::Context,
  done: impl Fn(&Viewer<Mock>) -> bool,
) {
  let deadline = Instant::now() + DEADLINE;
  loop {
    let _ = run_headless(ctx, raw(Vec::new()), |ui| viewer.poll(ui.ctx()));
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
  let mut input = raw(events);
  if viewer.viewing {
    input.viewport_id = viewer_id();
    input.viewports.insert(
      viewer_id(),
      egui::ViewportInfo {
        parent: Some(egui::ViewportId::ROOT),
        focused: Some(true),
        ..Default::default()
      },
    );
  }
  run_headless(ctx, input, |ui| {
    viewer.poll(ui.ctx());
    if viewer.viewing {
      viewer.show_viewer(ui);
    } else {
      viewer.show_setup(ui);
    }
  })
}

struct NativeFrame {
  root: egui::FullOutput,
  children: Vec<(egui::ViewportId, egui::FullOutput)>,
}

impl NativeFrame {
  fn commands(&self, id: egui::ViewportId) -> impl Iterator<Item = &egui::ViewportCommand> {
    std::iter::once(&self.root)
      .chain(self.children.iter().map(|(_, output)| output))
      .filter_map(move |output| output.viewport_output.get(&id))
      .flat_map(|output| &output.commands)
  }

  fn child(&self) -> &egui::FullOutput {
    self
      .viewport(viewer_id())
      .expect("native viewer must be rendered")
  }

  fn viewport(&self, id: egui::ViewportId) -> Option<&egui::FullOutput> {
    self
      .children
      .iter()
      .rev()
      .find_map(|(viewport, output)| (*viewport == id).then_some(output))
  }
}

/// Run setup and the deferred viewer in separate passes. The viewer’s
/// attached controls still use an immediate viewport.
fn native_frame(
  viewer: &mut Viewer<Mock>,
  ctx: &egui::Context,
  root_input: egui::RawInput,
  child_input: egui::RawInput,
) -> NativeFrame {
  native_frame_with_inputs(viewer, ctx, root_input, [(viewer_id(), child_input)])
}

fn native_frame_with_inputs<const N: usize>(
  viewer: &mut Viewer<Mock>,
  ctx: &egui::Context,
  root_input: egui::RawInput,
  inputs: [(egui::ViewportId, egui::RawInput); N],
) -> NativeFrame {
  ctx.set_embed_viewports(false);
  let mut inputs = HashMap::from(inputs);
  let child_input = inputs
    .remove(&viewer_id())
    .unwrap_or_else(|| raw(Vec::new()));
  let inputs = RefCell::new(inputs);
  let children = Rc::new(RefCell::new(Vec::new()));
  let recorded = Rc::clone(&children);
  egui::Context::set_immediate_viewport_renderer(move |ctx, mut viewport| {
    let mut input = inputs
      .borrow_mut()
      .entry(viewport.ids.this)
      .or_insert_with(|| raw(Vec::new()))
      .take();
    input.viewport_id = viewport.ids.this;
    input.viewports.entry(viewport.ids.this).or_default().parent = Some(viewport.ids.parent);
    let output = run_headless(ctx, input, viewport.viewport_ui_cb.as_mut());
    recorded.borrow_mut().push((viewport.ids.this, output));
  });
  let mut has_viewer = false;
  let root = run_headless(ctx, root_input, |ui| {
    viewer.logic(ui.ctx());
    let builder = viewer.setup(ui);
    has_viewer = builder.is_some();
    if let Some(builder) = builder {
      ui.ctx()
        .show_viewport_deferred(viewer_id(), builder, |_, _| {});
    }
  });
  if has_viewer {
    let child = run_headless(ctx, in_viewer_viewport(child_input), |ui| {
      viewer.viewer(ui, egui::ViewportClass::Deferred);
    });
    children.borrow_mut().push((viewer_id(), child));
  }
  NativeFrame {
    root,
    children: std::mem::take(&mut *children.borrow_mut()),
  }
}

fn in_viewer_viewport(mut input: egui::RawInput) -> egui::RawInput {
  input.viewport_id = viewer_id();
  input.viewports.entry(viewer_id()).or_default().parent = Some(egui::ViewportId::ROOT);
  input
}

fn close_input(id: egui::ViewportId) -> egui::RawInput {
  let mut input = raw(Vec::new());
  input
    .viewports
    .entry(id)
    .or_default()
    .events
    .push(egui::ViewportEvent::Close);
  input
}

fn rendered_image(output: &egui::FullOutput, texture: egui::TextureId) -> egui::Rect {
  output
    .shapes
    .iter()
    .find_map(|shape| match &shape.shape {
      egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
      _ => None,
    })
    .expect("preview image must be drawn")
}

fn rendered_label(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
  output.shapes.iter().find_map(|shape| match &shape.shape {
    egui::epaint::Shape::Text(text) if text.galley.text() == label => {
      Some(text.pos + text.galley.rect.center().to_vec2())
    }
    _ => None,
  })
}

fn label_center(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
  rendered_label(output, label).unwrap_or_else(|| panic!("expected rendered label {label:?}"))
}

fn click_setup_label(viewer: &mut Viewer<Mock>, ctx: &egui::Context, name: &str) {
  let at = label_center(&render(viewer, ctx, Vec::new()), name);
  render(
    viewer,
    ctx,
    vec![
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
    ],
  );
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
      phase: egui::TouchPhase::Move,
      source: egui::MouseWheelSource::Wheel,
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
fn deferred_viewer_uploads_new_frames_without_another_setup_pass() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  ctx.set_embed_viewports(false);
  let mut viewer = create(&mock, &ctx, true, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let capture = mock.captures().pop().unwrap();
  let stream = Arc::clone(&viewer.preview.as_ref().unwrap().stream);
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let mut app = view::App::new(viewer);
  let root = run_headless(&ctx, raw(Vec::new()), |ui| {
    app.logic(ui.ctx());
    app.update_ui(ui);
  });
  let viewport = &root.viewport_output[&viewer_id()];
  assert!(viewport.class == egui::ViewportClass::Deferred);
  let draw = viewport.viewport_ui_cb.as_ref().unwrap();
  let opening = run_headless(&ctx, in_viewer_viewport(raw(Vec::new())), |ui| draw(ui));
  assert!(
    opening.viewport_output[&egui::ViewportId::ROOT]
      .commands
      .contains(&egui::ViewportCommand::Visible(false))
  );
  assert!(rendered_image(&opening, texture).is_positive());
  let sent = input.sent();

  let (wake, repaints) = std::sync::mpsc::channel();
  ctx.set_repaint_observer(move |request| {
    let _ = wake.send(request.viewport_id);
  });
  let wait_for_repaint = |id, ready: &dyn Fn() -> bool| {
    let deadline = Instant::now() + DEADLINE;
    loop {
      let requested = repaints
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .expect("capture must wake the intended viewport");
      if requested == id && ready() {
        break;
      }
    }
  };
  for shade in [47, 93, 156] {
    while repaints.try_recv().is_ok() {}
    let before = stream.lock().unwrap().received;
    let mut frame = b"P6\n2 1\n255\n".to_vec();
    frame.extend([shade; 6]);
    capture.push_frame_bytes(&frame);
    wait_for_repaint(viewer_id(), &|| stream.lock().unwrap().received > before);
    let mut raw = in_viewer_viewport(raw(Vec::new()));
    raw
      .viewports
      .entry(egui::ViewportId::ROOT)
      .or_default()
      .occluded = Some(true);
    let mut output = ctx.run_ui(raw, |ui| draw(ui));
    let delta = output.textures_delta.set[&texture]
      .last()
      .expect("viewer must upload the new frame");
    let egui::ImageData::Color(image) = &delta.image;
    assert_eq!(image.pixels, [egui::Color32::from_gray(shade); 2]);
    assert!(rendered_image(&output, texture).is_positive());
    output.textures_delta.clear();
  }
  assert_eq!(input.sent(), sent);
  assert_eq!(mock.captures().len(), 1);

  // A closed source must also restore setup if the viewer gets no paint pass.
  while repaints.try_recv().is_ok() {}
  capture.finish();
  wait_for_repaint(egui::ViewportId::ROOT, &|| {
    stream.lock().unwrap().end.is_some()
  });
  let mut setup = raw(Vec::new());
  setup
    .viewports
    .entry(egui::ViewportId::ROOT)
    .or_default()
    .occluded = Some(true);
  let logic = ctx.run_logic(&setup, |ctx| app.logic(ctx));
  assert!(
    logic.viewport_commands[&egui::ViewportId::ROOT]
      .contains(&egui::ViewportCommand::Visible(true))
  );
  assert!(!app.is_viewing());
  assert!(input.closed() && input.cancelled());
}

#[test]
fn native_viewer_hides_setup_and_close_restores_it_without_reopening_capture() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let capture = mock.captures().pop().unwrap();
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let opened = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert_ne!(viewer_id(), egui::ViewportId::ROOT);
  assert!(opened.children.iter().all(|(id, _)| *id == viewer_id()));
  assert_eq!(
    opened.root.viewport_output[&viewer_id()].parent,
    egui::ViewportId::ROOT
  );
  assert_eq!(
    opened.root.viewport_output[&viewer_id()].builder.inner_size,
    Some(egui::vec2(1100.0, 550.0))
  );
  assert!(
    opened
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Visible(false)))
  );
  assert!(
    opened
      .commands(viewer_id())
      .any(|command| matches!(command, egui::ViewportCommand::Focus))
  );
  assert!(
    !opened
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_)))
  );
  assert!(
    !opened
      .child()
      .shapes
      .iter()
      .any(|shape| matches!(&shape.shape,
    egui::epaint::Shape::Text(text) if text.galley.text().contains("Back")))
  );

  // A hidden setup cannot replace the active source, even if a stale event
  // attempts a driver change or refresh while the child is running.
  let old_driver = viewer.driver.clone();
  let windows_queries = mock.records("windows").len();
  viewer.choose_driver("windows.wgc");
  viewer.load_target(&ctx);
  viewer.discover(&ctx);
  assert_eq!(viewer.driver, old_driver);
  assert_eq!(mock.records("windows").len(), windows_queries);
  assert!(!capture.closed() && !input.closed());

  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  let at = rendered_image(opened.child(), texture).center();
  native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    raw(interactions(at, true)),
  );
  assert!(input.sent().contains("key F8 1\n"));
  let before_close = input.sent();
  let mut closing = close_input(viewer_id());
  closing.events = interactions(at, false);
  let closed = native_frame(&mut viewer, &ctx, raw(Vec::new()), closing);
  assert!(!viewer.viewing);
  assert!(input.closed() && input.cancelled());
  assert_eq!(
    input.sent(),
    before_close,
    "a close event must not forward accompanying input"
  );
  assert!(!capture.closed() && !capture.cancelled());
  assert!(
    closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Visible(true)))
  );
  assert!(
    closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Minimized(false)))
  );
  assert!(
    closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Focus))
  );
  assert!(
    !closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Close))
  );
  assert!(ready(&viewer));
  let setup = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert!(setup.children.is_empty());
  label_center(&setup.root, "CAPTURE DRIVER");
  assert_eq!(mock.captures().len(), 1);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let reopened = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert!(
    reopened
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(command, egui::ViewportCommand::Visible(false)))
  );
  assert_eq!(mock.inputs().len(), 2);
  assert_eq!(mock.captures().len(), 1);
  native_frame(&mut viewer, &ctx, raw(Vec::new()), close_input(viewer_id()));
  viewer.choose_driver("windows.wgc");
  assert_eq!(
    viewer.driver, "windows.wgc",
    "closing the child unlocks setup"
  );
}

fn viewer_input(size: egui::Vec2, events: Vec<egui::Event>) -> egui::RawInput {
  egui::RawInput {
    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
    ..raw(events)
  }
}

fn wait_for_capture_end(viewer: &Viewer<Mock>) {
  let deadline = Instant::now() + DEADLINE;
  let stream = &viewer.preview.as_ref().unwrap().stream;
  while stream.lock().unwrap().end.is_none() {
    assert!(Instant::now() < deadline, "capture worker did not finish");
    thread::sleep(Duration::from_millis(2));
  }
}

#[test]
fn closed_remote_window_closes_the_viewer_and_refreshes_setup() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, true, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let capture = mock.captures().pop().unwrap();
  viewer.show_stats = true;
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));

  let queries = mock.records("windows").len();
  mock.0.config.lock().unwrap().first_window_closed = true;
  capture.finish();
  wait_for_capture_end(&viewer);
  let closed = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert!(!viewer.viewing && viewer.stats.is_none() && viewer.preview.is_none());
  assert!(closed.viewport(viewer_id()).is_none());
  for expected in [
    egui::ViewportCommand::Visible(true),
    egui::ViewportCommand::Focus,
  ] {
    assert!(
      closed
        .commands(egui::ViewportId::ROOT)
        .any(|cmd| *cmd == expected)
    );
  }
  label_center(&closed.root, "CAPTURE DRIVER");
  assert!(capture.closed() && capture.cancelled());
  assert!(input.closed() && input.cancelled());

  poll_until(&mut viewer, &ctx, |viewer| viewer.window_query.is_none());
  assert!(mock.records("windows").len() > queries);
  assert_eq!(viewer.selected_window.as_deref(), Some("window-1"));
  assert!(viewer.selected_window().is_none());
  assert!(
    viewer
      .windows
      .iter()
      .any(|source| source.window.id == "window-2")
  );
  assert_eq!(mock.captures().len(), 1);
}

#[test]
fn display_end_and_truncated_window_frames_keep_the_viewer_open() {
  for (window, tail, message) in [
    (false, &b""[..], "Capture ended."),
    (
      true,
      &b"P6\n8 8\n255\na"[..],
      "See terminal for remote diagnostics.",
    ),
  ] {
    let mock = Mock::default();
    let ctx = egui::Context::default();
    let mut viewer = create(&mock, &ctx, window, "auto");
    poll_until(&mut viewer, &ctx, ready);
    let capture = mock.captures().pop().unwrap();
    viewer.start(&ctx);
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().input.is_some()
    });
    let input = mock.inputs().pop().unwrap();
    native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
    capture.push_frame_bytes(tail);
    capture.finish();
    wait_for_capture_end(&viewer);
    let frame = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
    assert!(viewer.viewing && frame.viewport(viewer_id()).is_some());
    assert!(viewer.preview_error.as_deref().unwrap().contains(message));
    assert!(input.closed() && input.cancelled());
  }
}

#[test]
fn debug_overlay_reports_stream_pixels_and_leaves_video_input_active() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let setup = render(&mut viewer, &ctx, Vec::new());
  let checkbox = label_center(&setup, "Debug overlay");
  render(
    &mut viewer,
    &ctx,
    vec![
      egui::Event::PointerMoved(checkbox),
      primary_button(checkbox, true),
      primary_button(checkbox, false),
    ],
  );
  assert!(viewer.show_stats);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let remote = mock.inputs().pop().unwrap();
  let child = |size, events| {
    let mut input = viewer_input(size, events);
    input
      .viewports
      .entry(viewer_id())
      .or_default()
      .native_pixels_per_point = Some(2.0);
    input
  };
  let frame = |viewer: &mut Viewer<Mock>, size, events| {
    native_frame(viewer, &ctx, raw(Vec::new()), child(size, events))
  };
  let initial = frame(&mut viewer, egui::vec2(400.0, 200.0), Vec::new());
  let metrics = |frame: &NativeFrame, size: &str| {
    let text = frame
      .child()
      .shapes
      .iter()
      .find_map(|shape| match &shape.shape {
        egui::epaint::Shape::Text(text) if text.galley.text().starts_with("Download") => {
          assert!(
            shape
              .clip_rect
              .contains_rect(text.galley.rect.translate(text.pos.to_vec2()))
          );
          Some(text)
        }
        _ => None,
      })
      .expect("debug overlay must be drawn in the viewer");
    assert!(text.galley.text().contains(size));
    text.pos + egui::vec2(20.0, 6.0)
  };
  metrics(&initial, "Frame     2×1 px");
  let size = egui::vec2(320.0, 160.0);
  let resized = frame(&mut viewer, size, Vec::new());
  let at = metrics(&resized, "Frame     2×1 px");
  frame(&mut viewer, size, interactions(at, false));
  let wire = remote.sent();
  let buttons: Vec<_> = wire
    .lines()
    .filter(|line| line.starts_with("button 1 "))
    .collect();
  assert_eq!(buttons, ["button 1 1", "button 1 0"]);

  let capture = mock.captures().pop().unwrap();
  capture.push_frame_bytes(b"P6\n3 2\n255\nabcdefghijklmnopqr");
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer
      .preview
      .as_ref()
      .unwrap()
      .texture
      .as_ref()
      .unwrap()
      .size()
      == [3, 2]
  });
  let new_frame = frame(&mut viewer, size, Vec::new());
  metrics(&new_frame, "Frame     3×2 px");
  let narrow = frame(&mut viewer, egui::vec2(125.0, 250.0), Vec::new());
  metrics(&narrow, "Frame     3×2 px");
}

#[test]
fn expanded_diagnostics_copy_and_collapse_without_forwarding_input() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.show_stats = true;
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let remote = mock.inputs().pop().unwrap();
  let capture = mock.captures().pop().unwrap();
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  let frame_at_size = |viewer: &mut Viewer<Mock>, size, events| {
    native_frame(viewer, &ctx, raw(Vec::new()), viewer_input(size, events))
  };
  let frame =
    |viewer: &mut Viewer<Mock>, events| frame_at_size(viewer, egui::vec2(640.0, 400.0), events);
  let click = |viewer: &mut Viewer<Mock>, at| {
    frame(
      viewer,
      vec![egui::Event::PointerMoved(at), primary_button(at, true)],
    );
    frame(viewer, vec![primary_button(at, false)])
  };
  let floating_panel = |frame: &NativeFrame| {
    let video = rendered_image(frame.child(), texture);
    let panel = frame
      .child()
      .shapes
      .iter()
      .find_map(|shape| match &shape.shape {
        egui::epaint::Shape::Rect(rect)
          if rect.fill == egui::Color32::from_rgba_unmultiplied(12, 17, 25, 234) =>
        {
          Some(rect.rect)
        }
        _ => None,
      })
      .expect("the expanded diagnostics background must be drawn");
    assert!(video.contains_rect(panel));
    assert!(panel.left() - video.left() < video.right() - panel.right());
    assert!(video.bottom() - panel.bottom() < panel.top() - video.top());
    for label in ["Copy", "Collapse"] {
      assert!(panel.contains(label_center(frame.child(), label)));
    }
    panel
  };

  let initial = frame(&mut viewer, Vec::new());
  let video_point = rendered_image(initial.child(), texture).center();
  let held = frame(&mut viewer, interactions(video_point, true));
  let before_expand = remote.sent();
  assert!(before_expand.contains("key F8 1\n"));
  let expand = label_center(held.child(), "[expand]");
  click(&mut viewer, expand);
  let expanded = frame(&mut viewer, Vec::new());
  label_center(expanded.child(), "Diagnostics");
  let expanded_panel = floating_panel(&expanded);
  let wide_size = egui::vec2(1200.0, 700.0);
  frame_at_size(&mut viewer, wide_size, Vec::new());
  let wide = frame_at_size(&mut viewer, wide_size, Vec::new());
  let wide_panel = floating_panel(&wide);
  assert_eq!(
    rendered_image(wide.child(), texture),
    egui::Rect::from_min_size(egui::Pos2::ZERO, wide_size)
  );
  let details = label_center(wide.child(), "Capture driver");
  let charts = label_center(wide.child(), "Download");
  assert!(charts.x > details.x && (charts.y - details.y).abs() < 8.0);
  assert!(
    wide_panel.height() < 420.0,
    "the wide panel should fit its contents below the height limit: {wide_panel:?}"
  );
  let chart_bottom = wide
    .child()
    .shapes
    .iter()
    .filter_map(|shape| match &shape.shape {
      egui::epaint::Shape::Rect(rect) if rect.fill == egui::Color32::from_black_alpha(52) => {
        Some(rect.rect.bottom())
      }
      _ => None,
    })
    .max_by(f32::total_cmp)
    .expect("diagnostics charts must be drawn");
  assert!(
    (0.0..24.0).contains(&(wide_panel.bottom() - chart_bottom)),
    "the wide panel should leave only padding below its charts"
  );

  let narrow_size = egui::vec2(320.0, 200.0);
  frame_at_size(&mut viewer, narrow_size, Vec::new());
  let resized = frame_at_size(&mut viewer, narrow_size, Vec::new());
  let resized_panel = floating_panel(&resized);
  assert!(resized_panel.width() < expanded_panel.width());
  assert!(resized_panel.height() < expanded_panel.height());
  let detail = label_center(resized.child(), "Capture driver");
  assert!(resized_panel.contains(detail));
  let pointer = resized_panel.center();
  frame_at_size(
    &mut viewer,
    narrow_size,
    vec![egui::Event::PointerMoved(pointer)],
  );
  frame_at_size(
    &mut viewer,
    narrow_size,
    vec![egui::Event::MouseWheel {
      unit: egui::MouseWheelUnit::Point,
      delta: egui::vec2(0.0, -70.0),
      modifiers: egui::Modifiers::NONE,
      phase: egui::TouchPhase::Move,
      source: egui::MouseWheelSource::Wheel,
    }],
  );
  let scrolled = frame_at_size(&mut viewer, narrow_size, Vec::new());
  assert_eq!(floating_panel(&scrolled), resized_panel);
  assert!(
    !scrolled.child().shapes.iter().any(|shape| {
      matches!(&shape.shape, egui::epaint::Shape::Text(text)
        if text.galley.text() == "Capture driver"
          && text.pos.y + text.galley.rect.center().y >= detail.y)
    }),
    "diagnostics body should scroll inside the panel"
  );
  assert_eq!(&remote.sent()[before_expand.len()..], "reset\n");

  frame(&mut viewer, interactions(video_point, false));
  capture.push_frame_bytes(b"P6\n3 2\n255\nabcdefghijklmnopqr");
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().image_size() == Some([3, 2])
  });
  let local = frame(&mut viewer, Vec::new());
  let copy = label_center(local.child(), "Copy");
  let copied = click(&mut viewer, copy);
  let copied_text = copied
    .child()
    .platform_output
    .commands
    .iter()
    .find_map(|command| match command {
      egui::OutputCommand::CopyText(text) => Some(text),
      _ => None,
    })
    .expect("Copy must export the diagnostics through the viewer");
  assert!(copied_text.contains("Capture driver: windows.dxgi"));
  assert!(copied_text.contains("Resolution: 3 × 2 px"));
  for label in [
    "Frame format:",
    "Frames received:",
    "Preview frames:",
    "Superseded frames:",
    "Payload received:",
    "Payload sent:",
    "Download:",
    "Upload:",
    "Latency:",
    "Preview FPS:",
  ] {
    assert!(copied_text.contains(label), "diagnostics omitted {label}");
  }

  let collapse = label_center(copied.child(), "Collapse");
  click(&mut viewer, collapse);
  let compact = frame(&mut viewer, Vec::new());
  let expand = label_center(compact.child(), "[expand]");
  assert_eq!(&remote.sent()[before_expand.len()..], "reset\n");

  click(&mut viewer, expand);
  let expanded = frame(&mut viewer, Vec::new());
  label_center(expanded.child(), "Diagnostics");
  let key = |key, pressed| egui::Event::Key {
    key,
    physical_key: Some(key),
    pressed,
    repeat: false,
    modifiers: egui::Modifiers::NONE,
  };
  let before_escape = remote.sent();
  frame(
    &mut viewer,
    vec![key(egui::Key::F9, true), key(egui::Key::Escape, true)],
  );
  let escaped = frame(
    &mut viewer,
    vec![key(egui::Key::Escape, false), key(egui::Key::F9, false)],
  );
  label_center(escaped.child(), "[expand]");
  assert_eq!(remote.sent(), before_escape);

  frame(&mut viewer, interactions(video_point, false));
  assert!(remote.sent()[before_escape.len()..].contains("button 1 1\n"));
  assert!(!remote.closed() && !capture.closed());
  assert_eq!(mock.captures().len(), 1);
}

#[test]
fn viewer_controls_overlay_keeps_full_video_through_reveal_resize_and_hide() {
  use egui::ViewportCommand;

  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let mut pixels = b"P6\n40 20\n255\n".to_vec();
  pixels.extend([0; 40 * 20 * 3]);
  mock.captures()[0].push_frame_bytes(&pixels);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().image_size() == Some([40, 20])
  });
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  let video_size = egui::vec2(400.0, 200.0);
  let resized = egui::vec2(640.0, 320.0);
  let mut frame = |size, events| {
    native_frame(
      &mut viewer,
      &ctx,
      raw(Vec::new()),
      viewer_input(size, events),
    )
  };
  let check_video = |frame: &NativeFrame, size| {
    assert_eq!(
      rendered_image(frame.child(), texture),
      egui::Rect::from_min_size(egui::Pos2::ZERO, size)
    );
    assert!(!frame.commands(viewer_id()).any(|command| matches!(
      command,
      ViewportCommand::InnerSize(_) | ViewportCommand::OuterPosition(_)
    )));
  };
  let opened = frame(video_size, Vec::new());
  assert_eq!(
    opened.root.viewport_output[&viewer_id()]
      .builder
      .decorations,
    Some(false)
  );
  assert!(rendered_label(opened.child(), "ctream viewer").is_none());
  check_video(&opened, video_size);

  let edge = egui::pos2(100.0, crate::chrome::REVEAL_HEIGHT * 0.5);
  let revealing = frame(video_size, vec![egui::Event::PointerMoved(edge)]);
  check_video(&revealing, video_size);
  let shown = frame(video_size, Vec::new());
  check_video(&shown, video_size);
  assert!(label_center(shown.child(), "ctream viewer").y < crate::chrome::BAR_HEIGHT);
  let after_resize = frame(resized, Vec::new());
  check_video(&after_resize, resized);
  let close = label_center(after_resize.child(), "×");
  assert!(close.x > video_size.x && close.x < resized.x);

  let before_overlay = input.sent();
  frame(
    resized,
    vec![
      egui::Event::PointerMoved(close),
      egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
      },
    ],
  );
  assert_eq!(input.sent(), before_overlay, "overlay input stays local");

  let click = egui::pos2(resized.x * 0.8, resized.y * 0.4);
  let before = input.sent().len();
  frame(
    resized,
    vec![
      egui::Event::PointerMoved(click),
      primary_button(click, true),
      primary_button(click, false),
    ],
  );
  let wire = input.sent();
  assert!(wire[before..].contains("move 32 8 40 20\nbutton 1 1\n"));
  assert!(wire[before..].contains("button 1 0\n"));

  let hidden = frame(resized, vec![egui::Event::PointerGone]);
  check_video(&hidden, resized);
  assert!(rendered_label(hidden.child(), "ctream viewer").is_none());
}

fn primary_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
  egui::Event::PointerButton {
    pos,
    button: egui::PointerButton::Primary,
    pressed,
    modifiers: egui::Modifiers::NONE,
  }
}

#[test]
fn title_bar_debug_button_toggles_the_live_overlay_without_forwarding_clicks() {
  for attached in [false, true] {
    let mock = Mock::default();
    let ctx = egui::Context::default();
    let mut viewer = create(&mock, &ctx, false, "auto");
    poll_until(&mut viewer, &ctx, ready);
    viewer.start(&ctx);
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().input.is_some()
    });
    let capture = mock.captures().pop().unwrap();
    let remote = mock.inputs().pop().unwrap();
    let controls = crate::chrome::controls_id();
    native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));

    let frame = |viewer: &mut Viewer<Mock>, title_events| {
      let size = egui::vec2(crate::view::MIN_WINDOW.x, 400.0);
      let (video_events, panel_events) = if attached {
        viewer.chrome.test_attached_hover = Some(true);
        (Vec::new(), title_events)
      } else {
        (title_events, Vec::new())
      };
      let mut video = viewer_input(size, video_events);
      // Keep controls available in a maximized viewer as well.
      video.viewports.entry(viewer_id()).or_default().maximized = Some(true);
      let panel = viewer_input(egui::vec2(size.x, crate::chrome::BAR_HEIGHT), panel_events);
      native_frame_with_inputs(
        viewer,
        &ctx,
        raw(Vec::new()),
        [(viewer_id(), video), (controls, panel)],
      )
    };
    let has_overlay = |frame: &NativeFrame| {
      frame.child().shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::epaint::Shape::Text(text)
          if text.galley.text().starts_with("Download"))
      })
    };

    let reveal = egui::pos2(100.0, crate::chrome::REVEAL_HEIGHT * 0.5);
    frame(&mut viewer, vec![egui::Event::PointerMoved(reveal)]);
    let initial = frame(&mut viewer, Vec::new());
    let toolbar_id = if attached { controls } else { viewer_id() };
    let toolbar = initial
      .viewport(toolbar_id)
      .expect("the title bar must be rendered");
    // The icon sits one button width and a 12-point separator left of Minimize.
    let debug = label_center(toolbar, "−") - egui::vec2(crate::chrome::BUTTON_WIDTH + 12.0, 0.0);
    assert!(rendered_label(toolbar, "Debug").is_none());
    assert!(!has_overlay(&initial));
    assert!(viewer.stats.is_none());
    let before = remote.sent().len();

    for enabled in [true, false] {
      frame(&mut viewer, vec![egui::Event::PointerMoved(debug)]);
      let pressed = frame(&mut viewer, vec![primary_button(debug, true)]);
      assert!(
        !pressed
          .commands(viewer_id())
          .any(|command| *command == egui::ViewportCommand::StartDrag)
      );
      let clicked = frame(&mut viewer, vec![primary_button(debug, false)]);
      assert!(
        clicked
          .viewport(toolbar_id)
          .unwrap()
          .platform_output
          .events
          .iter()
          .any(|event| {
            matches!(event, egui::output::OutputEvent::Clicked(info)
          if info.label.as_deref() == Some("Debug overlay"))
          })
      );
      let updated = frame(&mut viewer, Vec::new());
      assert_eq!(has_overlay(&updated), enabled);
      assert_eq!(viewer.stats.is_some(), enabled);
      assert!(viewer.viewing);
    }

    if !attached {
      let key_event = |key, pressed| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
      };
      for (key, enabled) in [(egui::Key::Enter, true), (egui::Key::Space, false)] {
        // Focus the fourth title-bar button without clicking it.
        for _ in 0..4 {
          frame(
            &mut viewer,
            vec![
              key_event(egui::Key::Tab, true),
              key_event(egui::Key::Tab, false),
            ],
          );
        }
        let before_key = remote.sent().len();
        frame(&mut viewer, vec![key_event(key, true)]);
        let updated = frame(&mut viewer, vec![key_event(key, false)]);
        assert_eq!(has_overlay(&updated), enabled);
        assert!(
          !remote.sent()[before_key..].contains("key "),
          "the Debug button must keep its activation keys local"
        );
      }
    }

    assert!(!remote.sent()[before..].contains("button "));
    assert!(!remote.closed() && !remote.cancelled());
    assert!(!capture.closed() && !capture.cancelled());
    assert_eq!(mock.inputs().len(), 1);
    assert_eq!(mock.captures().len(), 1);
  }
}

#[test]
fn attached_controls_leave_the_video_fixed_and_handle_only_local_input() {
  use egui::ViewportCommand;

  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  let mut pixels = b"P6\n40 20\n255\n".to_vec();
  pixels.extend([0; 40 * 20 * 3]);
  let capture = mock.captures().pop().unwrap();
  capture.push_frame_bytes(&pixels);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().image_size() == Some([40, 20])
  });
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let remote = mock.inputs().pop().unwrap();
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  let size = egui::vec2(400.0, 200.0);
  let screen_rect = egui::Rect::from_min_size(egui::pos2(300.0, 240.0), size);
  let controls = crate::chrome::controls_id();
  let mut frame = |hovered, overlap, video_events, panel_events| {
    viewer.chrome.test_attached_hover = hovered;
    viewer.chrome.test_attached_overlap = overlap;
    let mut video = viewer_input(size, video_events);
    let info = video.viewports.entry(viewer_id()).or_default();
    info.inner_rect = Some(screen_rect);
    info.outer_rect = Some(screen_rect);
    let panel = viewer_input(egui::vec2(size.x, crate::chrome::BAR_HEIGHT), panel_events);
    native_frame_with_inputs(
      &mut viewer,
      &ctx,
      raw(Vec::new()),
      [(viewer_id(), video), (controls, panel)],
    )
  };
  let check_geometry = |frame: &NativeFrame| {
    assert_eq!(
      rendered_image(frame.child(), texture).translate(screen_rect.min.to_vec2()),
      screen_rect
    );
    assert!(!frame.commands(viewer_id()).any(|command| matches!(
      command,
      ViewportCommand::InnerSize(_) | ViewportCommand::OuterPosition(_)
    )));
  };

  let opened = frame(None, None, Vec::new(), Vec::new());
  check_geometry(&opened);
  assert!(opened.viewport(controls).is_none());
  let shown = frame(Some(true), None, Vec::new(), Vec::new());
  check_geometry(&shown);
  let panel = shown
    .viewport(controls)
    .expect("attached controls were drawn");
  let close = label_center(panel, "×");

  let top_pixel = egui::pos2(320.0, 3.0);
  let before = remote.sent().len();
  let clicked = frame(
    Some(true),
    None,
    vec![
      egui::Event::PointerMoved(top_pixel),
      primary_button(top_pixel, true),
      primary_button(top_pixel, false),
    ],
    Vec::new(),
  );
  check_geometry(&clicked);
  assert!(remote.sent()[before..].contains("move 32 0 40 20\nbutton 1 1\n"));

  let overlap = egui::Rect::from_min_max(
    egui::pos2(150.0, 0.0),
    egui::pos2(size.x, crate::chrome::BAR_HEIGHT),
  );
  let before_overlap = remote.sent();
  let overlapped = frame(
    Some(true),
    Some(overlap),
    vec![
      egui::Event::PointerMoved(top_pixel),
      primary_button(top_pixel, true),
      primary_button(top_pixel, false),
    ],
    Vec::new(),
  );
  check_geometry(&overlapped);
  assert!(overlapped.viewport(controls).is_some());
  assert_eq!(
    remote.sent(),
    before_overlap,
    "native overlay input stays local"
  );

  let outside = egui::pos2(80.0, 3.0);
  let clicked_outside = frame(
    Some(true),
    Some(overlap),
    vec![
      egui::Event::PointerMoved(outside),
      primary_button(outside, true),
      primary_button(outside, false),
    ],
    Vec::new(),
  );
  check_geometry(&clicked_outside);
  assert!(remote.sent()[before_overlap.len()..].contains("move 8 0 40 20\nbutton 1 1\n"));

  let hidden = frame(Some(false), Some(overlap), Vec::new(), Vec::new());
  check_geometry(&hidden);
  assert!(hidden.viewport(controls).is_none());

  let before_close = remote.sent();
  frame(
    Some(true),
    Some(overlap),
    Vec::new(),
    vec![egui::Event::PointerMoved(close)],
  );
  frame(
    Some(true),
    Some(overlap),
    Vec::new(),
    vec![primary_button(close, true)],
  );
  let closed = frame(
    Some(true),
    Some(overlap),
    Vec::new(),
    vec![primary_button(close, false)],
  );
  assert!(!viewer.viewing);
  assert!(remote.closed() && remote.cancelled());
  assert_eq!(remote.sent(), before_close, "panel clicks must stay local");
  assert!(!capture.closed() && !capture.cancelled());
  assert!(
    closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| *command == ViewportCommand::Visible(true))
  );
  let setup = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert!(setup.children.is_empty());
  label_center(&setup.root, "CAPTURE DRIVER");
}

#[test]
fn viewer_controls_stay_local_and_closing_restores_setup() {
  use egui::ViewportCommand;

  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let capture = mock.captures().pop().unwrap();
  let size = raw(Vec::new()).screen_rect.unwrap().size();
  let y = crate::chrome::BAR_HEIGHT * 0.5;
  let closed = {
    let mut frame = |events| {
      native_frame(
        &mut viewer,
        &ctx,
        raw(Vec::new()),
        viewer_input(size, events),
      )
    };
    frame(Vec::new());
    frame(vec![egui::Event::PointerMoved(egui::pos2(
      100.0,
      crate::chrome::REVEAL_HEIGHT * 0.5,
    ))]);
    let toolbar = frame(Vec::new());

    let drag = egui::pos2(100.0, y);
    let dragging = frame(vec![
      egui::Event::PointerMoved(drag),
      primary_button(drag, true),
    ]);
    assert!(
      dragging
        .commands(viewer_id())
        .any(|command| *command == ViewportCommand::StartDrag)
    );
    frame(vec![primary_button(drag, false)]);

    let mut click = |label| {
      let at = label_center(toolbar.child(), label);
      frame(vec![egui::Event::PointerMoved(at)]);
      frame(vec![primary_button(at, true)]);
      frame(vec![primary_button(at, false)])
    };
    let minimized = click("−");
    assert!(
      minimized
        .commands(viewer_id())
        .any(|command| *command == ViewportCommand::Minimized(true))
    );
    let maximized = click("□");
    assert!(
      maximized
        .commands(viewer_id())
        .any(|command| *command == ViewportCommand::Maximized(true))
    );
    click("×")
  };
  assert!(!viewer.viewing);
  assert!(input.closed() && input.cancelled());
  assert!(!input.sent().contains("button 1"));
  assert!(!capture.closed() && !capture.cancelled());
  assert!(
    closed
      .commands(egui::ViewportId::ROOT)
      .any(|command| *command == ViewportCommand::Visible(true))
  );
  let setup = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  assert!(setup.children.is_empty());
  label_center(&setup.root, "CAPTURE DRIVER");
}

#[test]
fn viewer_keeps_remote_drag_owned_by_video_across_the_top_edge() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let mut frame = |events| native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(events));
  let start = egui::pos2(250.0, 250.0);
  let end = egui::pos2(250.0, crate::chrome::REVEAL_HEIGHT * 0.5);
  frame(Vec::new());
  frame(vec![
    egui::Event::PointerMoved(start),
    primary_button(start, true),
  ]);
  let across_edge = frame(vec![egui::Event::PointerMoved(end)]);
  assert!(rendered_label(across_edge.child(), "ctream viewer").is_none());
  assert!(
    !across_edge
      .commands(viewer_id())
      .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_))),
    "a drag from the video must not resize the viewer"
  );
  let released = frame(vec![primary_button(end, false)]);
  assert!(!released.commands(viewer_id()).any(|command| matches!(
    command,
    egui::ViewportCommand::StartDrag | egui::ViewportCommand::BeginResize(_)
  )));
  let wire = input.sent();
  assert_eq!(
    wire
      .lines()
      .filter(|line| line.starts_with("button 1 "))
      .collect::<Vec<_>>(),
    ["button 1 1", "button 1 0"]
  );
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn viewer_resize_grip_does_not_forward_clicks() {
  use egui::ViewportCommand;

  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let edge = raw(Vec::new()).screen_rect.unwrap().max;
  let at = edge - egui::Vec2::splat(crate::chrome::RESIZE_GRIP * 0.5);
  let mut frame = |events| native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(events));
  frame(Vec::new());
  frame(vec![egui::Event::PointerMoved(at)]);
  let resizing = frame(vec![primary_button(at, true)]);
  frame(vec![primary_button(at, false)]);
  assert!(
    resizing.commands(viewer_id()).any(|command| {
      *command == ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast)
    })
  );
  assert!(!input.sent().contains("button 1"));
}

#[test]
fn viewer_input_isolated_from_setup_and_focus_loss_releases_held_state() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, true, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let opened = native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  let at = rendered_image(opened.child(), texture).center();
  let before = input.sent();
  native_frame(
    &mut viewer,
    &ctx,
    raw(interactions(at, false)),
    raw(Vec::new()),
  );
  assert_eq!(
    input.sent(),
    before,
    "root events must never reach the viewer's input channel"
  );
  native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    raw(interactions(at, true)),
  );
  assert!(input.sent().contains("key F8 1\n"));
  assert!(
    viewer
      .preview
      .as_ref()
      .unwrap()
      .input
      .as_ref()
      .unwrap()
      .is_forwarding()
  );

  // Painting another view of the shared texture must not reset the viewer's
  // held input, including when the read-only pane cannot fit an image.
  let held = input.sent();
  for size in [egui::vec2(860.0, 620.0), egui::vec2(1.0, 1.0)] {
    let mut source_input = raw(interactions(at, false));
    source_input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size));
    let _ = run_headless(&ctx, source_input, |ui| {
      egui::CentralPanel::default_margins().show(ui, |ui| {
        viewer
          .preview
          .as_mut()
          .unwrap()
          .show(ui, None, &[], PreviewMode::Setup);
      });
    });
    assert_eq!(input.sent(), held);
    assert!(
      viewer
        .preview
        .as_ref()
        .unwrap()
        .input
        .as_ref()
        .unwrap()
        .is_forwarding()
    );
  }
  let mut unfocused = raw(vec![egui::Event::WindowFocused(false)]);
  unfocused.focused = false;
  native_frame(&mut viewer, &ctx, raw(Vec::new()), unfocused.clone());
  assert!(input.sent().ends_with("reset\n"));
  assert!(
    !viewer
      .preview
      .as_ref()
      .unwrap()
      .input
      .as_ref()
      .unwrap()
      .is_forwarding()
  );
  let released = input.sent();
  native_frame(&mut viewer, &ctx, raw(Vec::new()), unfocused);
  assert_eq!(
    input.sent(),
    released,
    "idle unfocused paints must not repeatedly reset"
  );
  assert!(!input.closed());
  native_frame(&mut viewer, &ctx, raw(Vec::new()), close_input(viewer_id()));
  assert!(input.closed() && input.cancelled());
}

#[test]
fn resized_native_viewer_maps_clicks_using_its_own_image_rectangle() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, true, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let input = mock.inputs().pop().unwrap();
  let texture = viewer
    .preview
    .as_ref()
    .unwrap()
    .texture
    .as_ref()
    .unwrap()
    .id();
  for size in [egui::vec2(1000.0, 400.0), egui::vec2(400.0, 800.0)] {
    let mut child = raw(Vec::new());
    child.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size));
    let output = native_frame(&mut viewer, &ctx, raw(Vec::new()), child.clone());
    let rect = rendered_image(output.child(), texture);
    assert_eq!(rect.min, egui::Pos2::ZERO);
    assert_eq!(rect.max, egui::Pos2::ZERO + size);
    let at = rect.min + rect.size() * egui::vec2(0.75, 0.5);
    let before = input.sent().len();
    child.events = vec![
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
    ];
    native_frame(&mut viewer, &ctx, raw(Vec::new()), child);
    let sent = input.sent();
    assert!(
      sent[before..].contains("move 1 0 2 1\n"),
      "{size:?}: {}",
      &sent[before..]
    );
    assert!(sent[before..].contains("button 1 1\n"));
    assert!(sent[before..].contains("button 1 0\n"));
  }
}

#[test]
fn native_viewer_restores_the_aspect_after_local_and_source_resizes() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, true, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  let child = |width, height, time| egui::RawInput {
    screen_rect: Some(egui::Rect::from_min_size(
      egui::Pos2::ZERO,
      egui::vec2(width, height),
    )),
    time: Some(time),
    ..raw(Vec::new())
  };
  let initial = native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(1100.0, 550.0, 1.0),
  );
  assert_eq!(
    initial.root.viewport_output[&viewer_id()]
      .builder
      .inner_size,
    Some(egui::vec2(1100.0, 550.0))
  );

  let dragging = native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 550.0, 1.05),
  );
  assert!(
    !dragging
      .commands(viewer_id())
      .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_)))
  );
  let settled = native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 550.0, 1.25),
  );
  assert!(settled.commands(viewer_id()).any(|command| matches!(command, egui::ViewportCommand::InnerSize(size) if *size == egui::vec2(800.0, 400.0))));
  let unchanged = native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 550.0, 1.40),
  );
  assert!(
    !unchanged
      .commands(viewer_id())
      .any(|command| matches!(command, egui::ViewportCommand::InnerSize(_)))
  );
  native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 400.0, 1.45),
  );

  mock.captures()[0].push_frame_bytes(b"P6\n2 2\n255\nabcdefghijkl");
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().image_size() == Some([2, 2])
  });
  native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 400.0, 1.50),
  );
  let source_resized = native_frame(
    &mut viewer,
    &ctx,
    raw(Vec::new()),
    child(800.0, 400.0, 1.70),
  );
  assert!(source_resized.commands(viewer_id()).any(|command| matches!(command, egui::ViewportCommand::InnerSize(size) if *size == egui::vec2(800.0, 800.0))));
}

#[test]
fn closing_setup_shuts_down_the_viewer_and_capture_without_restoring_setup() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "auto");
  poll_until(&mut viewer, &ctx, ready);
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  let capture = mock.captures().pop().unwrap();
  let input = mock.inputs().pop().unwrap();
  native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  let output = native_frame(
    &mut viewer,
    &ctx,
    close_input(egui::ViewportId::ROOT),
    raw(Vec::new()),
  );
  assert!(!viewer.viewing && viewer.preview.is_none());
  assert!(capture.closed() && capture.cancelled());
  assert!(input.closed() && input.cancelled());
  assert!(output.children.is_empty());
  assert!(
    !output
      .commands(egui::ViewportId::ROOT)
      .any(|command| matches!(
        command,
        egui::ViewportCommand::Visible(true) | egui::ViewportCommand::CancelClose
      ))
  );
}

#[test]
fn welcome_interactions_stay_local_and_start_close_reuse_the_capture() {
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
    viewer.close_viewer(&ctx);
    assert!(
      input.closed() && input.cancelled(),
      "Viewer close must close the channel holding F8"
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
  viewer.choose_driver("windows.wgc");
  viewer.load_target(&ctx);
  poll_until(&mut viewer, &ctx, |_| {
    mock
      .records("windows")
      .iter()
      .any(|record| record.target() == Some("windows-2"))
  });
  let stale_query = mock
    .records("windows")
    .into_iter()
    .find(|record| record.target() == Some("windows-2"))
    .unwrap();
  viewer.choose_driver("windows.dxgi");
  viewer.load_target(&ctx);
  poll_until(&mut viewer, &ctx, ready);
  let current = Arc::clone(&viewer.preview.as_ref().unwrap().stream);
  capture_gate.release();
  windows_gate.release();
  poll_until(&mut viewer, &ctx, |_| {
    stale.closed() && stale.cancelled() && stale_query.closed() && stale_query.cancelled()
  });
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert!(viewer.windows.is_empty());
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
fn video_setting_restarts_only_setup_capture_and_keeps_the_selected_window() {
  use ctream_proto::VideoCodec;

  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, true, "windows.wgc");
  poll_until(&mut viewer, &ctx, ready);
  let original = mock.captures().pop().unwrap();
  let discovery_queries = mock.records("discover").len();
  let catalog_queries = mock.records("capture-drivers").len();
  let window_queries = mock.records("windows").len();
  let image_size = viewer.preview.as_ref().unwrap().image_size();

  let output = render(&mut viewer, &ctx, Vec::new());
  for label in [
    "VIDEO STREAM",
    "AV1",
    "Balanced",
    "Max FPS",
    "Video: Raw RGB24",
  ] {
    label_center(&output, label);
  }
  click_setup_label(&mut viewer, &ctx, "AV1");
  click_setup_label(&mut viewer, &ctx, "H.264");
  assert_eq!(viewer.video.codec, VideoCodec::H264);
  assert!(original.closed() && original.cancelled());
  assert!(!viewer.viewing && mock.inputs().is_empty());

  poll_until(&mut viewer, &ctx, ready);
  let captures = mock.captures();
  assert_eq!(captures.len(), 2);
  assert_eq!(captures[1].arguments, original.arguments);
  assert_eq!(viewer.driver, "windows.wgc");
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert_eq!(viewer.selected_window.as_deref(), Some("window-1"));
  assert_eq!(viewer.preview.as_ref().unwrap().image_size(), image_size);
  assert_eq!(mock.records("discover").len(), discovery_queries);
  assert_eq!(mock.records("capture-drivers").len(), catalog_queries);
  assert_eq!(mock.records("windows").len(), window_queries);

  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  assert!(!viewer.set_video_options(VideoOptions::default()));
  assert_eq!(viewer.video.codec, VideoCodec::H264);
  assert!(!captures[1].closed() && !captures[1].cancelled());
  assert_eq!(mock.inputs().len(), 1);
}

#[test]
fn close_during_input_handshake_cancels_control_without_stopping_capture() {
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
  native_frame(&mut viewer, &ctx, raw(Vec::new()), raw(Vec::new()));
  native_frame(&mut viewer, &ctx, raw(Vec::new()), close_input(viewer_id()));
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
fn requested_window_is_never_broadened_by_display_driver_metadata() {
  for (window_mode, window) in [(false, Some("window-1")), (true, None)] {
    let mock = Mock::default();
    let ctx = egui::Context::default();
    let mut viewer = Viewer::new(
      mock.clone(),
      Some(Target::Windows(1)),
      window_mode,
      window.map(str::to_owned),
      "windows.dxgi".into(),
      &ctx,
    );
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.catalog_ready && viewer.discovery.is_none() && viewer.window_query.is_none()
    });
    assert!(viewer.window_mode);
    assert_eq!(viewer.selected_window.as_deref(), window);
    assert!(
      viewer
        .selection_error()
        .unwrap()
        .contains("requested source type")
    );
    assert!(viewer.connecting.is_none() && mock.captures().is_empty());
    viewer.start(&ctx);
    assert!(!viewer.viewing && mock.inputs().is_empty());

    // Selecting this same card is an explicit change of scope, unlike merely
    // receiving its catalog metadata after a command-line window selection.
    click_setup_label(&mut viewer, &ctx, "DXGI");
    assert!(!viewer.window_mode);
    poll_until(&mut viewer, &ctx, ready);
    assert_eq!(mock.captures().len(), 1);
    assert_eq!(mock.captures()[0].arguments[3..], ["windows.dxgi"]);
    assert!(mock.inputs().is_empty());
  }
}

#[test]
fn catalog_and_card_choices_wait_for_display_discovery_before_opening_sources() {
  let mock = Mock::default();
  let discovery = mock.delay("discover", "");
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(
    mock.clone(),
    Some(Target::Windows(3)),
    false,
    None,
    "windows.wgc".into(),
    &ctx,
  );
  poll_until(&mut viewer, &ctx, |viewer| viewer.catalog_ready);
  assert!(viewer.discovery.is_some());
  assert!(viewer.window_mode);
  assert_eq!(viewer.selected, Some(Target::Windows(3)));
  assert!(viewer.client.is_none() && viewer.requests.is_none());
  assert!(viewer.window_query.is_none());
  assert!(mock.records("windows").is_empty() && mock.captures().is_empty());

  // The driver list remains usable while discovery is pending, but choosing a
  // card must not start queries against an unvalidated, preselected display.
  click_setup_label(&mut viewer, &ctx, "DXGI");
  assert_eq!(viewer.driver, "windows.dxgi");
  assert!(!viewer.window_mode);
  assert!(viewer.client.is_none() && viewer.requests.is_none());
  assert!(viewer.window_query.is_none());
  assert!(mock.records("windows").is_empty() && mock.captures().is_empty());
  click_setup_label(&mut viewer, &ctx, "WGC");
  assert_eq!(viewer.driver, "windows.wgc");
  assert!(viewer.window_mode);

  discovery.release();
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert_eq!(viewer.selected_window.as_deref(), Some("window-1"));
  assert_eq!(viewer.windows.len(), 2);
  assert_eq!(mock.captures().len(), 1);
  assert!(mock.inputs().is_empty());
}

#[test]
fn unavailable_driver_or_missing_window_never_opens_capture() {
  for (driver, legacy) in [
    ("custom.missing", false),
    ("windows.wgc", false),
    ("windows.dxgi", true),
  ] {
    let mock = Mock::default();
    mock.0.config.lock().unwrap().legacy = legacy;
    let ctx = egui::Context::default();
    let mut viewer = Viewer::new(
      mock.clone(),
      Some(Target::Windows(1)),
      false,
      (driver == "windows.wgc").then(|| "missing-window".into()),
      driver.into(),
      &ctx,
    );
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.source_loaded && viewer.window_query.is_none()
    });
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

#[test]
fn complete_driver_catalog_and_tags_are_shown_before_selecting_a_source() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().catalog = Some(concat!(
    r#"[{"id":"custom.desktop","name":"Custom desktop","scopes":["display"],"systems":["windows"]},"#,
    r#"{"id":"custom.surface","name":"Custom surface","scopes":["window"],"systems":["wayland"]}]"#,
  ).into());
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(mock.clone(), None, false, None, "auto".into(), &ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.catalog_ready && viewer.discovery.is_none()
  });
  assert_eq!(viewer.drivers.len(), 2);
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert!(mock.captures().is_empty() && mock.inputs().is_empty());
  // Inspect the available drivers before the automatic display preview starts.
  viewer.preview_after = Instant::now() + DEADLINE;
  let output = render(&mut viewer, &ctx, Vec::new());
  for label in [
    "Custom desktop",
    "Custom surface",
    "Windows",
    "Linux",
    "Wayland",
    "Display",
    "Window",
    "No sources",
  ] {
    label_center(&output, label);
  }
  click_setup_label(&mut viewer, &ctx, "Custom surface");
  assert_eq!(viewer.driver, "custom.surface");
  assert!(viewer.window_mode && viewer.selected.is_none());
  assert!(
    viewer
      .selection_error()
      .unwrap()
      .contains("No compatible sources")
  );
  assert_eq!(
    viewer.drivers.len(),
    2,
    "unavailable providers remain visible"
  );
  assert!(mock.captures().is_empty() && mock.inputs().is_empty());
  mock.0.config.lock().unwrap().displays = Some(
    r#"[{"display":{"windows":{"number":1,"name":"inaccessible","bounds":[0,0,4,2]}},"class":null,"selectable":false}]"#.into(),
  );
  viewer.choose_driver("custom.desktop");
  viewer.discover(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.catalog_ready && viewer.discovery.is_none()
  });
  assert!(
    viewer
      .selection_error()
      .unwrap()
      .contains("No compatible sources")
  );
  mock.0.config.lock().unwrap().displays = Some("[]".into());
  viewer.discover(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.catalog_ready && viewer.discovery.is_none()
  });
  assert!(viewer.displays.is_empty());
  assert_eq!(
    viewer.drivers.len(),
    2,
    "driver discovery does not need a display"
  );
  assert!(mock.captures().is_empty() && mock.inputs().is_empty());
}

#[test]
fn driver_cards_set_the_scope_and_keep_the_complete_catalog() {
  let mock = Mock::default();
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "windows.dxgi");
  poll_until(&mut viewer, &ctx, ready);
  let original = mock.captures().pop().unwrap();
  let catalog_queries = mock.records("capture-drivers").len();
  click_setup_label(&mut viewer, &ctx, "WGC");
  assert_eq!(viewer.driver, "windows.wgc");
  assert!(viewer.window_mode);
  assert!(original.closed() && original.cancelled());
  assert!(!ready(&viewer));
  assert_eq!(viewer.drivers.len(), 2);
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(viewer.selected_window.as_deref(), Some("window-1"));
  let window_capture = mock.captures().pop().unwrap();
  assert_eq!(window_capture.arguments[3..], ["windows.wgc", "window-1"]);
  click_setup_label(&mut viewer, &ctx, "DXGI");
  assert_eq!(viewer.driver, "windows.dxgi");
  assert!(!viewer.window_mode);
  assert!(window_capture.closed() && window_capture.cancelled());
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(
    mock.captures().pop().unwrap().arguments[3..],
    ["windows.dxgi"]
  );
  assert_eq!(mock.records("capture-drivers").len(), catalog_queries);
  assert_eq!(viewer.drivers.len(), 2);
  assert!(mock.inputs().is_empty());
}

#[test]
fn desktop_window_selection_uses_one_query_and_its_target_for_capture_and_input() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().desktop_windows = true;
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(mock.clone(), None, false, None, "windows.wgc".into(), &ctx);
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(viewer.selected, Some(Target::WindowsDesktop));
  assert_eq!(viewer.selected_window.as_deref(), Some("window-desktop"));
  let queries = mock.records("windows");
  assert_eq!(queries.len(), 1);
  assert_eq!(queries[0].arguments, ["1.0.9", "windows", "windows"]);
  assert_eq!(
    mock.captures()[0].arguments,
    [
      "1.0.9",
      "capture-with-driver",
      "windows",
      "windows.wgc",
      "window-desktop"
    ]
  );
  viewer.start(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| {
    viewer.preview.as_ref().unwrap().input.is_some()
  });
  assert_eq!(
    mock.inputs()[0].arguments,
    ["1.0.9", "input-window", "windows", "window-desktop"]
  );
}

#[test]
fn macos_catalog_defaults_to_a_window_and_enables_input_only_on_start() {
  for driver in ["auto", "macos.screencapturekit"] {
    let mock = Mock::default();
    {
      let mut config = mock.0.config.lock().unwrap();
      config.displays =
        Some(r#"[{"display":"macos_desktop","class":null,"selectable":true}]"#.into());
      config.catalog = Some(
        r#"[{"id":"macos.screencapturekit","name":"ScreenCaptureKit","scopes":["window"],"systems":["macos"]}]"#
          .into(),
      );
    }
    let ctx = egui::Context::default();
    let mut viewer = Viewer::new(mock.clone(), None, false, None, driver.into(), &ctx);
    poll_until(&mut viewer, &ctx, ready);
    assert_eq!(viewer.available_scopes(), [CaptureScope::Window]);
    assert_eq!(viewer.selected, Some(Target::MacosDesktop));
    assert_eq!(
      mock.records("windows")[0].arguments,
      ["1.0.11", "windows", "macos"]
    );
    assert_eq!(
      mock.captures()[0].arguments,
      [
        "1.0.11",
        "capture-with-driver",
        "macos",
        driver,
        "macos-31-42"
      ]
    );
    assert!(mock.inputs().is_empty());
    viewer.start(&ctx);
    poll_until(&mut viewer, &ctx, |viewer| {
      viewer.preview.as_ref().unwrap().input.is_some()
    });
    let input = mock.inputs().pop().unwrap();
    assert_eq!(
      input.arguments,
      ["1.0.11", "input-window", "macos", "macos-31-42"]
    );
    viewer.close_viewer(&ctx);
    assert!(input.closed() && input.cancelled());
  }
}

#[test]
fn display_driver_automatically_skips_an_unavailable_first_display() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().displays = Some(concat!(
    r#"[{"display":{"windows":{"number":1,"name":"blocked","bounds":[0,0,4,2]}},"class":null,"selectable":false},"#,
    r#"{"display":{"windows":{"number":2,"name":"available","bounds":[4,0,8,2]}},"class":null,"selectable":true}]"#,
  ).into());
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(mock.clone(), None, false, None, "windows.dxgi".into(), &ctx);
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(viewer.selected, Some(Target::Windows(2)));
  assert_eq!(mock.captures()[0].target(), Some("windows-2"));
  assert!(mock.records("windows").is_empty() && mock.inputs().is_empty());
}

#[test]
fn custom_driver_metadata_controls_compatibility_and_multiple_scopes() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().catalog = Some(concat!(
    r#"[{"id":"linux.user_backend","name":"Custom multiple sources","scopes":["display","window"],"systems":["windows"]},"#,
    r#"{"id":"windows.user_backend","name":"Custom other system","scopes":["window"],"systems":["x11"]}]"#,
  ).into());
  let ctx = egui::Context::default();
  let mut viewer = create(&mock, &ctx, false, "linux.user_backend");
  poll_until(&mut viewer, &ctx, ready);
  assert_eq!(
    viewer.available_scopes(),
    [CaptureScope::Display, CaptureScope::Window]
  );
  assert!(viewer.compatible(Target::Windows(2)));
  assert!(!viewer.compatible(Target::X11(7)));
  let original = mock.captures().pop().unwrap();
  viewer.window_mode = true;
  viewer.selected_window = Some("window-1".into());
  viewer.load_target(&ctx);
  poll_until(&mut viewer, &ctx, ready);
  assert!(original.closed() && original.cancelled());
  let window_capture = mock.captures().pop().unwrap();
  assert_eq!(
    window_capture.arguments[3..],
    ["linux.user_backend", "window-1"]
  );
  viewer.choose_driver("windows.user_backend");
  viewer.load_target(&ctx);
  assert!(viewer.selected.is_none() && viewer.selected_window.is_none());
  assert!(viewer.preview.is_none());
  assert!(window_capture.closed() && window_capture.cancelled());
  assert!(viewer.compatible(Target::X11(7)));
  assert!(!viewer.compatible(Target::Windows(2)));
  assert_eq!(viewer.drivers.len(), 2);
  assert!(mock.inputs().is_empty());
}

#[test]
fn refreshing_discovery_ignores_a_late_global_catalog() {
  let mock = Mock::default();
  let gate = mock.delay("capture-drivers", "");
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(mock.clone(), None, false, None, "auto".into(), &ctx);
  poll_until(&mut viewer, &ctx, |_| {
    !mock.records("capture-drivers").is_empty()
  });
  let stale = mock.records("capture-drivers").pop().unwrap();
  mock.0.config.lock().unwrap().catalog = Some(
    r#"[{"id":"custom.fresh","name":"Fresh catalog","scopes":["display"],"systems":["windows"]}]"#
      .into(),
  );
  viewer.discover(&ctx);
  poll_until(&mut viewer, &ctx, |viewer| viewer.catalog_ready);
  assert_eq!(viewer.drivers.len(), 1);
  assert_eq!(viewer.drivers[0].info.id, "custom.fresh");
  gate.release();
  poll_until(&mut viewer, &ctx, |_| stale.closed() && stale.cancelled());
  assert_eq!(viewer.drivers.len(), 1);
  assert_eq!(viewer.drivers[0].info.id, "custom.fresh");
  assert_eq!(mock.records("capture-drivers").len(), 2);
  assert!(mock.captures().is_empty() && mock.inputs().is_empty());
}

#[test]
fn targeted_catalog_fallback_merges_discovered_sources_before_selection() {
  let mock = Mock::default();
  mock.0.config.lock().unwrap().per_target_catalog_only = true;
  let ctx = egui::Context::default();
  let mut viewer = Viewer::new(mock.clone(), None, false, None, "auto".into(), &ctx);
  poll_until(&mut viewer, &ctx, |viewer| viewer.catalog_ready);
  assert_eq!(viewer.drivers.len(), 2);
  assert_eq!(viewer.selected, Some(Target::Windows(1)));
  assert!(mock.captures().is_empty());
  let catalogs = mock.records("capture-drivers");
  assert_eq!(catalogs.len(), 4);
  assert_eq!(catalogs[0].arguments, ["1.0.11", "capture-drivers"]);
  assert_eq!(catalogs[1].arguments, ["1.0.8", "capture-drivers"]);
  for target in ["windows-1", "windows-2"] {
    assert!(
      catalogs
        .iter()
        .any(|catalog| catalog.arguments == ["1.0.7", "capture-drivers", target])
    );
  }
  viewer.choose_driver("windows.wgc");
  viewer.selected = Some(Target::Windows(2));
  viewer.selected_window = Some("window-2".into());
  viewer.load_target(&ctx);
  poll_until(&mut viewer, &ctx, ready);
  let capture = mock.captures().pop().unwrap();
  assert_eq!(
    capture.arguments,
    [
      "1.0.7",
      "capture-with-driver",
      "windows-2",
      "windows.wgc",
      "window-2"
    ]
  );
  assert_eq!(
    viewer.preview.as_ref().unwrap().driver.as_deref(),
    Some("windows.wgc")
  );
  assert_eq!(mock.records("capture-drivers").len(), 4);
  assert!(mock.inputs().is_empty());
}
