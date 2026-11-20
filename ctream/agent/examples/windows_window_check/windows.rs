use crate::fixture::{self, Action, Event, Geometry, PhysicalPixels, Request, Result, Ui};
use ::windows::Win32::{
  Foundation::{HWND, POINT},
  UI::{
    Input::KeyboardAndMouse::{
      GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
      MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput, VK_F8, VK_LBUTTON,
    },
    WindowsAndMessaging::{GetCursorPos, GetForegroundWindow, SetCursorPos, WindowFromPoint},
  },
};
use ctream_client::{
  Client,
  geometry::{ImageGeometry, Point, Rect},
  input::Input,
  media::{Frame, VideoSource},
  transport::{Cancellation, Channel, Local, Transport},
};
use ctream_proto::{Display, Target};
use serde_json::json;
use std::{
  io::{self, Write},
  path::PathBuf,
  sync::{Arc, Mutex, mpsc},
  thread,
  time::{Duration, Instant},
};

type Children = Arc<Mutex<Vec<Cancellation>>>;
type LocalInput = Input<<Local as Transport>::Reader, <Local as Transport>::Writer>;

fn cancel_children(children: &Children) {
  for child in children
    .lock()
    .unwrap_or_else(|error| error.into_inner())
    .iter()
  {
    child.cancel();
  }
}

struct CheckedLocal {
  local: Local,
  children: Children,
}

impl Transport for CheckedLocal {
  type Reader = <Local as Transport>::Reader;
  type Writer = <Local as Transport>::Writer;

  fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
    let channel = self.local.open(arguments)?;
    self
      .children
      .lock()
      .unwrap_or_else(|error| error.into_inner())
      .push(channel.cancellation.clone());
    Ok(channel)
  }
}

fn request(ui: &mpsc::Sender<Request>, action: Action) -> Result<()> {
  let (reply, result) = mpsc::channel();
  ui.send(Request { action, reply })?;
  result
    .recv_timeout(Duration::from_secs(2))?
    .map_err(io::Error::other)?;
  Ok(())
}

fn milestone(stage: &str, details: serde_json::Value) {
  eprintln!("{}", json!({"stage": stage, "details": details}));
}

fn expect_event(
  events: &mpsc::Receiver<Event>,
  stage: &str,
  expected: impl Fn(&Event) -> bool,
) -> Result<()> {
  let deadline = Instant::now() + Duration::from_secs(2);
  let mut recent = std::collections::VecDeque::new();
  loop {
    let event = events
      .recv_timeout(deadline.saturating_duration_since(Instant::now()))
      .map_err(|error| format!("{stage}: {error}; recent events: {recent:?}"))?;
    if expected(&event) {
      milestone(stage, json!({"event": format!("{event:?}")}));
      return Ok(());
    }
    if matches!(
      event,
      Event::Click { target: false, .. } | Event::Key { target: false, .. }
    ) {
      return Err(format!("{stage}: input reached the covering window: {event:?}").into());
    }
    if matches!(event, Event::Closed) {
      return Err(format!("{stage}: target window was closed unexpectedly").into());
    }
    if recent.len() == 8 {
      recent.pop_front();
    }
    recent.push_back(event);
  }
}

struct RestoreInput {
  cursor: POINT,
  last_cursor: Option<POINT>,
  held_f8: bool,
  held_left: bool,
}

impl RestoreInput {
  fn new() -> Result<Self> {
    if unsafe { GetAsyncKeyState(i32::from(VK_F8.0)) } < 0
      || unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0
    {
      return Err("F8 or the left mouse button is already held; no input was sent".into());
    }
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor) }.map_err(|error| {
      format!("cannot read the interactive desktop cursor before input: {error}")
    })?;
    Ok(Self {
      cursor,
      last_cursor: None,
      held_f8: false,
      held_left: false,
    })
  }
}

impl Drop for RestoreInput {
  fn drop(&mut self) {
    // Failure cleanup releases only input this fixture pressed.
    if self.held_f8 {
      let release = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
          ki: KEYBDINPUT {
            wVk: VK_F8,
            dwFlags: KEYEVENTF_KEYUP,
            ..Default::default()
          },
        },
      };
      unsafe { SendInput(&[release], std::mem::size_of::<INPUT>() as i32) };
    }
    if self.held_left {
      let release = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
          mi: MOUSEINPUT {
            dwFlags: MOUSEEVENTF_LEFTUP,
            ..Default::default()
          },
        },
      };
      unsafe { SendInput(&[release], std::mem::size_of::<INPUT>() as i32) };
    }
    if let Some(last) = self.last_cursor {
      let mut current = POINT::default();
      if unsafe { GetCursorPos(&mut current) }.is_ok()
        && current.x.abs_diff(last.x) <= 1
        && current.y.abs_diff(last.y) <= 1
      {
        let _ = unsafe { SetCursorPos(self.cursor.x, self.cursor.y) };
      }
    }
  }
}

fn focus_owned(
  ui: &mpsc::Sender<Request>,
  hwnd: HWND,
  target: bool,
  events: &mpsc::Receiver<Event>,
  restore: &mut RestoreInput,
) -> Result<()> {
  let action = if target {
    Action::FocusTarget
  } else {
    Action::FocusCover
  };
  if request(ui, action).is_ok() && unsafe { GetForegroundWindow() } == hwnd {
    return Ok(());
  }
  // Windows may deny programmatic foreground activation to an SSH-launched process.
  // Activate only our visible fixture with one guarded click during the input stage.
  if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
    return Err("left button became held before fixture activation; no click sent".into());
  }
  let geometry = Geometry::read(hwnd)?;
  let client = geometry.sample();
  let expected = geometry.screen(client);
  if unsafe { WindowFromPoint(expected) } != hwnd {
    return Err("owned activation point is obscured; no click sent".into());
  }
  unsafe { SetCursorPos(expected.x, expected.y) }
    .map_err(|error| format!("cannot position the cursor over the owned fixture: {error}"))?;
  let mut actual = POINT::default();
  unsafe { GetCursorPos(&mut actual) }?;
  if actual.x.abs_diff(expected.x) > 1 || actual.y.abs_diff(expected.y) > 1 {
    return Err("activation cursor did not reach the owned fixture; no click sent".into());
  }
  restore.last_cursor = Some(actual);
  if unsafe { WindowFromPoint(actual) } != hwnd {
    return Err("activation cursor is no longer over the owned fixture; no click sent".into());
  }
  let click = [MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP].map(|flags| INPUT {
    r#type: INPUT_MOUSE,
    Anonymous: INPUT_0 {
      mi: MOUSEINPUT {
        dwFlags: flags,
        ..Default::default()
      },
    },
  });
  restore.held_left = true;
  if unsafe { SendInput(&click, std::mem::size_of::<INPUT>() as i32) } != 2 {
    return Err("Windows rejected the owned fixture activation click".into());
  }
  // Consume both activation messages; they must never satisfy the agent click checks.
  for pressed in [true, false] {
    expect_event(
      events,
      if pressed {
        "activation_down"
      } else {
        "activation_up"
      },
      |event| {
        matches!(event, Event::Click { target: actual_target, x, y, down }
        if *actual_target == target && *down == pressed && x.abs_diff(client.x) <= 1 && y.abs_diff(client.y) <= 1)
      },
    )?;
  }
  restore.held_left = false;
  if unsafe { GetForegroundWindow() } != hwnd {
    return Err("the guarded activation click did not focus the owned fixture".into());
  }
  milestone(
    "owned_activation",
    json!({"target": target, "guarded_click": true}),
  );
  Ok(())
}

fn verify_colors(
  capture: &mut impl VideoSource<Buffer = Vec<u8>>,
  geometry: &Geometry,
  palette: usize,
  stage: &str,
) -> Result<usize> {
  let mut seen = [false; 2];
  let mut frame = Frame::default();
  let [x, y] = geometry
    .frame_point(geometry.sample())
    .map(|value| value as usize);
  let mut last_pixel = None;
  let start = Instant::now();
  for count in 1..=45 {
    capture
      .read_frame(&mut frame)
      .map_err(|error| format!("{stage}: {error}"))?;
    // A resize can leave old-size frames queued in the pipe or frame pool.
    if frame.size == geometry.size.map(|value| value as usize) {
      let offset = (y * frame.size[0] + x) * 3;
      let pixel = frame
        .pixels
        .get(offset..offset + 3)
        .ok_or("fixture sample is outside capture")?;
      last_pixel = Some(pixel.to_vec());
      for (seen, color) in seen.iter_mut().zip(fixture::COLORS[palette]) {
        *seen |= pixel == color;
      }
      if seen.iter().all(|seen| *seen) {
        milestone(
          stage,
          json!({"frames": count, "frame_size": frame.size, "colors_verified": true}),
        );
        return Ok(count);
      }
    }
    if start.elapsed() > Duration::from_secs(5) {
      break;
    }
  }
  Err(format!("{stage}: expected frame size {:?} and both fixture colors; last size {:?}, pixel {last_pixel:?}, seen {seen:?}", geometry.size, frame.size).into())
}

fn verify_eof(capture: &mut impl VideoSource<Buffer = Vec<u8>>, stage: &str) -> Result<()> {
  let start = Instant::now();
  let mut frame = Frame::default();
  while capture.read_frame(&mut frame).is_ok() {
    if start.elapsed() > Duration::from_secs(3) {
      return Err(format!("{stage}: frame stream did not terminate").into());
    }
  }
  if start.elapsed() > Duration::from_secs(3) {
    return Err(format!("{stage}: termination exceeded three seconds").into());
  }
  milestone(stage, json!({"elapsed_ms": start.elapsed().as_millis()}));
  Ok(())
}

fn resize(ui: &mpsc::Sender<Request>, hwnd: HWND, size: [i32; 2]) -> Result<Geometry> {
  let old = Geometry::read(hwnd)?.size;
  request(ui, Action::Resize(size))?;
  let start = Instant::now();
  loop {
    let current = Geometry::read(hwnd)?;
    if current.size != old {
      return Ok(current);
    }
    if start.elapsed() > Duration::from_secs(1) {
      return Err("fixture resize did not change physical bounds".into());
    }
    thread::sleep(Duration::from_millis(10));
  }
}

fn verify_click(
  input: &mut LocalInput,
  hwnd: HWND,
  geometry: &Geometry,
  viewport: [f64; 2],
  events: &mpsc::Receiver<Event>,
  restore: &mut RestoreInput,
  index: usize,
) -> Result<()> {
  let expected = POINT {
    x: geometry.client.right / 4 * (index as i32 + 1),
    y: geometry.client.bottom / 3,
  };
  let screen = geometry.screen(expected);
  if unsafe { GetForegroundWindow() } != hwnd || unsafe { WindowFromPoint(screen) } != hwnd {
    return Err("fixture is not foreground and unobscured; click was not sent".into());
  }
  let area = Rect::new(
    Point::new(20.0, 30.0),
    Point::new(20.0 + viewport[0], 30.0 + viewport[1]),
  )
  .ok_or("invalid viewport")?;
  let image = ImageGeometry::fit(area, geometry.size).ok_or("invalid image geometry")?;
  let fitted = image.viewport();
  let expected_frame = geometry.frame_point(expected);
  // Use the pixel center, avoiding rounding onto the preceding pixel at an edge.
  let preview = Point::new(
    fitted.min().x
      + (f64::from(expected_frame[0]) + 0.5) / f64::from(geometry.size[0])
        * (fitted.max().x - fitted.min().x),
    fitted.min().y
      + (f64::from(expected_frame[1]) + 0.5) / f64::from(geometry.size[1])
        * (fitted.max().y - fitted.min().y),
  );
  let [x, y] = image
    .pixel(preview)
    .ok_or("preview point is outside image")?;
  input.move_pointer(x, y, geometry.size)?;
  let deadline = Instant::now() + Duration::from_secs(1);
  let actual = loop {
    let mut actual = POINT::default();
    unsafe { GetCursorPos(&mut actual) }?;
    if actual.x.abs_diff(screen.x) <= 1 && actual.y.abs_diff(screen.y) <= 1 {
      break actual;
    }
    if Instant::now() >= deadline {
      return Err(
        format!(
          "click_{}: requested screen [{}, {}], actual [{}, {}]; no click sent",
          index + 1,
          screen.x,
          screen.y,
          actual.x,
          actual.y
        )
        .into(),
      );
    }
    thread::sleep(Duration::from_millis(5));
  };
  restore.last_cursor = Some(actual);
  if unsafe { WindowFromPoint(actual) } != hwnd || unsafe { GetForegroundWindow() } != hwnd {
    return Err("actual cursor is not over the foreground fixture; click was not sent".into());
  }
  restore.held_left = true;
  input.button(1, true)?;
  input.button(1, false)?;
  for pressed in [true, false] {
    expect_event(
      events,
      &format!(
        "click_{}_{}",
        index + 1,
        if pressed { "down" } else { "up" }
      ),
      |event| matches!(event, Event::Click { target: true, x, y, down } if *down == pressed && x.abs_diff(expected.x) <= 1 && y.abs_diff(expected.y) <= 1),
    )?;
  }
  restore.held_left = false;
  milestone(
    "preview_geometry",
    json!({"viewport": viewport, "capture_size": geometry.size,
    "expected_client": [expected.x, expected.y], "actual_screen": [actual.x, actual.y]}),
  );
  Ok(())
}

struct Exercise {
  client: Client<CheckedLocal>,
  number: u32,
  title: String,
  handles: [usize; 2],
  events: mpsc::Receiver<Event>,
  ui: mpsc::Sender<Request>,
  serve: Option<u64>,
}

impl Exercise {
  fn run(self) -> Result<serde_json::Value> {
    let _physical = PhysicalPixels::enter()?;
    let hwnd = HWND(self.handles[0] as *mut _);
    let cover = HWND(self.handles[1] as *mut _);
    let windows = self.client.windows(Target::Windows(self.number))?;
    let window = windows
      .into_iter()
      .find(|window| window.title == self.title && window.app_id == fixture::CLASS)
      .ok_or("window discovery did not return the owned fixture title and class")?;
    milestone(
      "discovery",
      json!({"window_id": window.id, "title": window.title, "class": window.app_id}),
    );
    if let Some(seconds) = self.serve {
      println!(
        "{}",
        json!({"status": "serving", "display": format!("windows-{}", self.number),
        "window_id": window.id, "title": window.title, "seconds": seconds})
      );
      io::stdout().flush()?;
      let end = Instant::now() + Duration::from_secs(seconds);
      while Instant::now() < end {
        if matches!(
          self.events.recv_timeout(Duration::from_millis(100)),
          Ok(Event::Closed)
        ) {
          break;
        }
      }
      return Ok(json!({"status": "serve_complete", "automatic_input": false}));
    }
    let session = self
      .client
      .session(Target::Windows(self.number), Some(window));
    let catalog = self.client.capture_drivers(Target::Windows(self.number))?;
    if !catalog.iter().any(|driver| {
      driver.id == "windows.wgc" && driver.scopes.contains(&ctream_proto::CaptureScope::Window)
    }) {
      return Err("Windows capture catalog does not offer WGC window capture".into());
    }
    let capture = session.capture_with_driver("auto")?;
    if capture.driver() != Some("windows.wgc") {
      return Err("automatic Windows window capture did not select WGC".into());
    }
    milestone(
      "capture_driver",
      json!({"requested": "auto", "actual": capture.driver()}),
    );
    let (mut capture, lifetime) = capture.split();
    let original = Geometry::read(hwnd)?;
    verify_colors(&mut capture, &original, 0, "capture_initial")?;
    let grown = resize(&self.ui, hwnd, [640, 440])?;
    if grown.size[0] <= original.size[0] || grown.size[1] <= original.size[1] {
      return Err("grow did not increase both dimensions".into());
    }
    verify_colors(&mut capture, &grown, 0, "capture_grow")?;
    let shrunk = resize(&self.ui, hwnd, [360, 260])?;
    if shrunk.size[0] >= original.size[0] || shrunk.size[1] >= original.size[1] {
      return Err("shrink did not decrease both dimensions".into());
    }
    verify_colors(&mut capture, &shrunk, 0, "capture_shrink")?;
    request(&self.ui, Action::Cover(true))?;
    if unsafe { WindowFromPoint(shrunk.screen(shrunk.sample())) } != cover {
      return Err("opaque fixture did not cover the selected window".into());
    }
    verify_colors(&mut capture, &shrunk, 1, "capture_covered")?;

    let explicit = session.capture_with_driver("windows.wgc")?;
    if explicit.driver() != Some("windows.wgc") {
      return Err("explicit Windows window capture did not select WGC".into());
    }
    let (mut explicit, explicit_lifetime) = explicit.split();
    verify_colors(&mut explicit, &shrunk, 1, "capture_explicit_covered")?;
    drop(explicit_lifetime);
    drop(explicit);

    // Focus changes begin only at the explicit input stage, and only to our own windows.
    let mut restore = RestoreInput::new()?;
    focus_owned(&self.ui, cover, false, &self.events, &mut restore)?;
    let mut input = session.input()?;
    let mut before = POINT::default();
    unsafe { GetCursorPos(&mut before) }?;
    let [x, y] = shrunk.frame_point(shrunk.sample());
    input.move_pointer(x, y, shrunk.size)?;
    thread::sleep(Duration::from_millis(150));
    let mut after = POINT::default();
    unsafe { GetCursorPos(&mut after) }?;
    if unsafe { GetForegroundWindow() } != cover || before.x != after.x || before.y != after.y {
      return Err("hover over a covered target changed foreground or cursor".into());
    }
    milestone(
      "covered_hover",
      json!({"cover_remained_foreground": true, "cursor_unchanged": true}),
    );
    request(&self.ui, Action::Cover(false))?;
    focus_owned(&self.ui, hwnd, true, &self.events, &mut restore)?;
    let viewports = [[1000.0, 400.0], [400.0, 800.0]];
    verify_click(
      &mut input,
      hwnd,
      &shrunk,
      viewports[0],
      &self.events,
      &mut restore,
      0,
    )?;
    let resized_input = resize(&self.ui, hwnd, [500, 340])?;
    verify_colors(
      &mut capture,
      &resized_input,
      1,
      "capture_resize_during_input",
    )?;
    verify_click(
      &mut input,
      hwnd,
      &resized_input,
      viewports[1],
      &self.events,
      &mut restore,
      1,
    )?;

    if unsafe { GetForegroundWindow() } != hwnd {
      return Err("fixture lost foreground before keyboard test".into());
    }
    restore.held_f8 = true;
    input.key("F8", true)?;
    expect_event(
      &self.events,
      "key_down",
      |event| matches!(event, Event::Key { target: true, code, down: true } if *code == usize::from(VK_F8.0)),
    )?;
    drop(lifetime);
    verify_eof(&mut capture, "capture_eof_while_key_held")?;
    drop(input);
    expect_event(
      &self.events,
      "key_up_after_input_eof",
      |event| matches!(event, Event::Key { target: true, code, down: false } if *code == usize::from(VK_F8.0)),
    )?;
    restore.held_f8 = false;

    let capture = session.capture_with_driver("windows.wgc")?;
    if capture.driver() != Some("windows.wgc") {
      return Err("explicit WGC selection was not retained".into());
    }
    let (mut closing_capture, closing_lifetime) = capture.split();
    verify_colors(&mut closing_capture, &resized_input, 1, "capture_reopen")?;
    let mut closing_input = session.input()?;
    if unsafe { GetForegroundWindow() } != hwnd {
      return Err("fixture lost foreground before target-close keyboard test".into());
    }
    restore.held_f8 = true;
    closing_input.key("F8", true)?;
    expect_event(
      &self.events,
      "key_down_before_target_close",
      |event| matches!(event, Event::Key { target: true, code, down: true } if *code == usize::from(VK_F8.0)),
    )?;
    request(&self.ui, Action::CloseTarget)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { GetAsyncKeyState(i32::from(VK_F8.0)) } < 0 {
      if Instant::now() >= deadline {
        return Err("closing target did not release F8 while input stdin remained open".into());
      }
      thread::sleep(Duration::from_millis(10));
    }
    restore.held_f8 = false;
    milestone(
      "target_close_releases_input",
      json!({"input_stdin_open": true, "f8_released": true}),
    );
    verify_eof(&mut closing_capture, "target_close_ends_capture")?;
    drop(closing_input);
    drop(closing_lifetime);
    Ok(json!({"display": format!("windows-{}", self.number),
      "frame_sizes": [original.size, grown.size, shrunk.size, resized_input.size],
      "capture_driver": "windows.wgc", "automatic_and_explicit_driver_verified": true,
      "selected_window_colors_verified": true, "opaque_cover_verified": true,
      "covered_hover_preserves_focus": true, "click_viewport_sizes": viewports,
      "capture_eof_while_key_held_verified": true, "input_eof_releases_key_verified": true,
      "target_close_releases_key_verified": true, "target_close_ends_capture_verified": true}))
  }
}

pub fn run() -> Result<()> {
  const USAGE: &str =
    "usage: windows-window-check AGENT_EXECUTABLE windows-N [--serve SECONDS (1..90)]";
  let mut args = std::env::args_os().skip(1);
  let agent = PathBuf::from(args.next().ok_or(USAGE)?);
  let target = args.next().ok_or(USAGE)?;
  let Target::Windows(number) = Target::parse(target.to_str().ok_or(USAGE)?)? else {
    return Err("an explicit windows-N display is required".into());
  };
  let serve = match args.next() {
    Some(flag) if flag == "--serve" => {
      let value = args.next().ok_or(USAGE)?;
      let seconds: u64 = value.to_str().ok_or(USAGE)?.parse()?;
      if !(1..=90).contains(&seconds) {
        return Err(USAGE.into());
      }
      Some(seconds)
    }
    None => None,
    _ => return Err(USAGE.into()),
  };
  if args.next().is_some() {
    return Err(USAGE.into());
  }
  let _physical = PhysicalPixels::enter()?;
  let duration = serve.unwrap_or(20);
  let children = Children::default();
  let watchdog_children = Arc::clone(&children);
  let (_watchdog_done, watchdog_wait) = mpsc::channel::<()>();
  thread::spawn(move || {
    if matches!(
      watchdog_wait.recv_timeout(Duration::from_secs(duration + 5)),
      Err(mpsc::RecvTimeoutError::Timeout)
    ) {
      cancel_children(&watchdog_children);
      thread::sleep(Duration::from_millis(1200));
      std::process::exit(124);
    }
  });
  let client = Client::new(CheckedLocal {
    local: Local { agent },
    children: Arc::clone(&children),
  });
  let bounds = client
    .discover()?
    .into_iter()
    .find_map(|session| match session.display {
      Display::Windows {
        number: candidate,
        bounds,
        ..
      } if candidate == number
        && session.selectable
        && bounds[2] - bounds[0] >= 800
        && bounds[3] - bounds[1] >= 600 =>
      {
        Some(bounds)
      }
      _ => None,
    })
    .ok_or("selected display is unavailable or smaller than 800x600 physical pixels")?;
  let title = format!("ctream window capture fixture {}", std::process::id());
  let (events, received) = mpsc::channel();
  let ui = Ui::create(bounds, &title, events)?;
  let handles = ui.handles();
  let (requests, pending) = mpsc::channel::<Request>();
  let (finished, result) = mpsc::channel();
  let worker = thread::spawn(move || {
    let outcome = Exercise {
      client,
      number,
      title,
      handles,
      events: received,
      ui: requests,
      serve,
    }
    .run()
    .map_err(|error| error.to_string());
    let _ = finished.send(outcome);
  });
  let deadline = Instant::now() + Duration::from_secs(duration + u64::from(serve.is_some()));
  let outcome = loop {
    fixture::pump_messages();
    while let Ok(request) = pending.try_recv() {
      let response = ui.handle(request.action).map_err(|error| error.to_string());
      let _ = request.reply.send(response);
    }
    if let Ok(result) = result.try_recv() {
      break result;
    }
    if Instant::now() >= deadline {
      break Err("Windows window check exceeded its deadline".into());
    }
    thread::sleep(Duration::from_millis(5));
  };
  cancel_children(&children);
  drop(ui);
  let _ = worker.join();
  thread::sleep(Duration::from_millis(1100));
  let report = outcome.map_err(io::Error::other)?;
  serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
  println!();
  Ok(())
}
