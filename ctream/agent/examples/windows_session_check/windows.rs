use ::windows::{
  Win32::{
    Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{
      BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
      InvalidateRect, PAINTSTRUCT,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
      HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
      },
      Input::KeyboardAndMouse::{
        GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
        MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput, VK_F8, VK_LBUTTON,
      },
      WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
        GetClientRect, GetClipCursor, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW,
        IsWindow, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SW_SHOW, SetCursorPos,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, WM_DESTROY,
        WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_PAINT, WM_QUIT, WM_TIMER, WNDCLASSW,
        WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW,
      },
    },
  },
  core::{Error as WindowsError, w},
};
use ctream_client::{
  Client,
  geometry::{ImageGeometry, Point, Rect},
  media::{Frame, VideoSource},
  transport::{Cancellation, Channel, Local, Transport},
};
use ctream_proto::{Display, Target};
use serde_json::json;
use std::{
  error::Error,
  io,
  path::PathBuf,
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread,
  time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Children = Arc<Mutex<Vec<Cancellation>>>;
const COLORS: [[u8; 3]; 2] = [[42, 176, 96], [64, 112, 192]];

fn cancel_children(children: &Children) {
  for child in children
    .lock()
    .unwrap_or_else(|error| error.into_inner())
    .iter()
  {
    child.cancel();
  }
}

/// Retain cancellation handles so the overall deadline can interrupt a blocked reader.
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

struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
  fn enter() -> Result<Self> {
    let old = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if old.0.is_null() {
      return Err(WindowsError::from_thread().into());
    }
    Ok(Self(old))
  }
}

impl Drop for PhysicalPixels {
  fn drop(&mut self) {
    unsafe { SetThreadDpiAwarenessContext(self.0) };
  }
}

#[derive(Debug)]
enum Event {
  Click { x: i32, y: i32, down: bool },
  Key { code: usize, down: bool },
  Closed,
}

struct FixtureState {
  events: mpsc::Sender<Event>,
  phase: AtomicBool,
}

unsafe extern "system" fn window_proc(
  hwnd: HWND,
  message: u32,
  wparam: WPARAM,
  lparam: LPARAM,
) -> LRESULT {
  let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const FixtureState;
  if !state.is_null() {
    // The main thread owns the box until after DestroyWindow returns.
    let state = unsafe { &*state };
    match message {
      WM_TIMER => {
        state.phase.fetch_xor(true, Ordering::Relaxed);
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        return LRESULT(0);
      }
      WM_PAINT => {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        let [r, g, b] = COLORS[usize::from(state.phase.load(Ordering::Relaxed))];
        let brush = unsafe {
          CreateSolidBrush(COLORREF(
            u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16),
          ))
        };
        unsafe { FillRect(dc, &paint.rcPaint, brush) };
        let _ = unsafe { DeleteObject(brush.into()) };
        let _ = unsafe { EndPaint(hwnd, &paint) };
        return LRESULT(0);
      }
      WM_LBUTTONDOWN | WM_LBUTTONUP => {
        let _ = state.events.send(Event::Click {
          x: (lparam.0 as u16 as i16).into(),
          y: ((lparam.0 >> 16) as u16 as i16).into(),
          down: message == WM_LBUTTONDOWN,
        });
        return LRESULT(0);
      }
      WM_KEYDOWN | WM_KEYUP => {
        let _ = state.events.send(Event::Key {
          code: wparam.0,
          down: message == WM_KEYDOWN,
        });
        return LRESULT(0);
      }
      WM_DESTROY => {
        let _ = state.events.send(Event::Closed);
      }
      _ => {}
    }
  }
  unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct Fixture {
  hwnd: HWND,
  _state: Box<FixtureState>,
}

impl Fixture {
  fn create(bounds: [i32; 4], events: mpsc::Sender<Event>) -> Result<Self> {
    let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }?.0);
    let class = WNDCLASSW {
      lpfnWndProc: Some(window_proc),
      hInstance: instance,
      lpszClassName: w!("CtreamWindowsSessionFixture"),
      ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
      return Err(WindowsError::from_thread().into());
    }
    let state = Box::new(FixtureState {
      events,
      phase: AtomicBool::new(false),
    });
    let hwnd = unsafe {
      CreateWindowExW(
        WS_EX_TOPMOST,
        class.lpszClassName,
        w!("ctream input and capture test"),
        WS_OVERLAPPEDWINDOW,
        bounds[0] + 40,
        bounds[1] + 40,
        400,
        280,
        None,
        None,
        Some(instance),
        None,
      )
    }?;
    unsafe {
      SetWindowLongPtrW(
        hwnd,
        GWLP_USERDATA,
        (&*state as *const FixtureState) as isize,
      )
    };
    let fixture = Self {
      hwnd,
      _state: state,
    };
    if unsafe { SetTimer(Some(hwnd), 1, 200, None) } == 0 {
      return Err(WindowsError::from_thread().into());
    }
    let _ = unsafe { ShowWindow(hwnd, SW_SHOW) };
    let _ = unsafe { SetForegroundWindow(hwnd) };
    Ok(fixture)
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    if unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
      let _ = unsafe { DestroyWindow(self.hwnd) };
    }
  }
}

struct RestoreInput {
  cursor: POINT,
  last_cursor: Option<POINT>,
  held_f8: bool,
  held_left: bool,
}

impl Drop for RestoreInput {
  fn drop(&mut self) {
    if self.held_f8 {
      // Failure cleanup releases only the key this fixture pressed.
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

fn expect_event(
  events: &mpsc::Receiver<Event>,
  stage: &str,
  expected: impl Fn(&Event) -> bool,
) -> Result<()> {
  let deadline = Instant::now() + Duration::from_secs(2);
  let mut observed = std::collections::VecDeque::new();
  loop {
    let event = match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
      Ok(event) => event,
      Err(error) => {
        let mut cursor = POINT::default();
        let cursor = unsafe { GetCursorPos(&mut cursor) }
          .ok()
          .map(|()| [cursor.x, cursor.y]);
        return Err(
          format!("{stage}: {error}; recent events: {observed:?}; cursor: {cursor:?}").into(),
        );
      }
    };
    if matches!(event, Event::Closed) {
      return Err(format!("{stage}: fixture was closed during input checks").into());
    }
    if expected(&event) {
      eprintln!(
        "{}",
        json!({"stage": stage, "status": "verified", "event": format!("{event:?}")})
      );
      return Ok(());
    }
    if observed.len() == 8 {
      observed.pop_front();
    }
    observed.push_back(event);
    if Instant::now() >= deadline {
      return Err(format!("{stage}: timed out; recent events: {observed:?}").into());
    }
  }
}

fn exercise(
  client: Client<CheckedLocal>,
  number: u32,
  bounds: [i32; 4],
  hwnd_value: usize,
  events: mpsc::Receiver<Event>,
) -> Result<serde_json::Value> {
  let _physical = PhysicalPixels::enter()?;
  let hwnd = HWND(hwnd_value as *mut _);
  if unsafe { GetAsyncKeyState(i32::from(VK_F8.0)) } < 0 {
    return Err("F8 is already held; retry after releasing it".into());
  }
  if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } < 0 {
    return Err("left mouse button is already held; retry after releasing it".into());
  }
  let mut cursor = POINT::default();
  unsafe { GetCursorPos(&mut cursor) }?;
  let mut restore = RestoreInput {
    cursor,
    last_cursor: None,
    held_f8: false,
    held_left: false,
  };
  let mut client_rect = RECT::default();
  unsafe { GetClientRect(hwnd, &mut client_rect) }?;
  if client_rect.right < 16 || client_rect.bottom < 16 {
    return Err("fixture client area is too small".into());
  }
  let client_points = [
    POINT {
      x: client_rect.right / 4,
      y: client_rect.bottom / 3,
    },
    POINT {
      x: client_rect.right * 3 / 4,
      y: client_rect.bottom * 2 / 3,
    },
  ];
  let mut screen_points = client_points;
  for point in &mut screen_points {
    if !unsafe { ClientToScreen(hwnd, point) }.as_bool() {
      return Err(WindowsError::from_thread().into());
    }
  }
  let size = [
    (bounds[2] - bounds[0]) as u32,
    (bounds[3] - bounds[1]) as u32,
  ];
  let session = client.session(Target::Windows(number), None);
  let (mut capture, lifetime) = session.capture()?.split();
  let mut frame = Frame::default();
  let mut colors_seen = [false; 2];
  let mut frame_count = 0;
  while !colors_seen.iter().all(|seen| *seen) {
    capture.read_frame(&mut frame)?;
    if frame.size != size.map(|value| value as usize) {
      return Err("capture dimensions do not match discovery".into());
    }
    frame_count += 1;
    let x = (screen_points[0].x - bounds[0]) as usize;
    let y = (screen_points[0].y - bounds[1]) as usize;
    let offset = (y * frame.size[0] + x) * 3;
    let pixel = frame
      .pixels
      .get(offset..offset + 3)
      .ok_or("fixture is outside the captured display")?;
    for (seen, color) in colors_seen.iter_mut().zip(COLORS) {
      *seen |= pixel == color;
    }
    if frame_count > 40 {
      return Err("capture did not observe both fixture colors".into());
    }
  }
  eprintln!(
    "{}",
    json!({"stage": "capture", "status": "verified", "frames": frame_count, "frame_size": size})
  );
  drop(lifetime);
  let closed = Instant::now();
  while capture.read_frame(&mut frame).is_ok() {
    if closed.elapsed() > Duration::from_secs(3) {
      return Err("capture did not close after EOF".into());
    }
  }
  if closed.elapsed() > Duration::from_secs(3) {
    return Err("capture cleanup exceeded the deadline".into());
  }
  eprintln!(
    "{}",
    json!({"stage": "capture_eof", "status": "verified", "elapsed_ms": closed.elapsed().as_millis()})
  );

  let mut input = session.input()?;
  eprintln!("{}", json!({"stage": "input_handshake", "status": "ready"}));
  let viewports = [[1000.0, 400.0], [400.0, 800.0]];
  for (index, ((viewport, screen), expected)) in viewports
    .into_iter()
    .zip(screen_points)
    .zip(client_points)
    .enumerate()
  {
    // Verify the target belongs to this fixture before injecting any click.
    if unsafe { ::windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(screen) } != hwnd {
      return Err("fixture click point is obscured; input was not sent".into());
    }
    let area = Rect::new(
      Point::new(20.0, 30.0),
      Point::new(20.0 + viewport[0], 30.0 + viewport[1]),
    )
    .ok_or("invalid viewport")?;
    let image = ImageGeometry::fit(area, size).ok_or("invalid image geometry")?;
    let fitted = image.viewport();
    let point = Point::new(
      fitted.min().x
        + f64::from(screen.x - bounds[0]) / f64::from(size[0]) * (fitted.max().x - fitted.min().x),
      fitted.min().y
        + f64::from(screen.y - bounds[1]) / f64::from(size[1]) * (fitted.max().y - fitted.min().y),
    );
    let [x, y] = image.pixel(point).ok_or("preview point is outside image")?;
    if index == 0 {
      let mut clipping = RECT::default();
      let clip = unsafe { GetClipCursor(&mut clipping) }
        .map(|()| json!([clipping.left, clipping.top, clipping.right, clipping.bottom]))
        .unwrap_or_else(|error| json!({"error": format!("{error}")}));
      eprintln!(
        "{}",
        json!({
          "stage": "before_first_move",
          "foreground_matches_fixture": unsafe { GetForegroundWindow() } == hwnd,
          "cursor_clip": clip,
        })
      );
    }
    input.move_pointer(x, y, size)?;
    let move_deadline = Instant::now() + Duration::from_secs(1);
    let actual = loop {
      let mut actual = POINT::default();
      unsafe { GetCursorPos(&mut actual) }?;
      if actual.x.abs_diff(screen.x) <= 1 && actual.y.abs_diff(screen.y) <= 1 {
        break actual;
      }
      if Instant::now() >= move_deadline {
        return Err(format!("pointer_{}: requested screen [{}, {}] from frame [{x}, {y}], actual [{}, {}]; click was not sent",
          index + 1, screen.x, screen.y, actual.x, actual.y).into());
      }
      thread::sleep(Duration::from_millis(5));
    };
    restore.last_cursor = Some(actual);
    if unsafe { ::windows::Win32::UI::WindowsAndMessaging::WindowFromPoint(actual) } != hwnd {
      return Err("actual pointer is not over the fixture; click was not sent".into());
    }
    eprintln!(
      "{}",
      json!({"stage": format!("click_{}_sent", index + 1), "viewport": viewport,
      "expected_client": [expected.x, expected.y], "expected_screen": [screen.x, screen.y],
        "frame_point": [x, y], "actual_cursor": [actual.x, actual.y],
      "fixture_foreground": unsafe { GetForegroundWindow() } == hwnd})
    );
    restore.held_left = true;
    input.button(1, true)?;
    input.button(1, false)?;
    for pressed in [true, false] {
      let stage = format!(
        "click_{}_{}",
        index + 1,
        if pressed { "down" } else { "up" }
      );
      expect_event(&events, &stage, |event| {
        matches!(event, Event::Click { x, y, down }
          if *down == pressed && x.abs_diff(expected.x) <= 1 && y.abs_diff(expected.y) <= 1)
      })?;
    }
    restore.held_left = false;
  }
  if unsafe { GetForegroundWindow() } != hwnd {
    return Err("fixture lost focus; keyboard input was not sent".into());
  }
  restore.held_f8 = true;
  input.key("F8", true)?;
  eprintln!("{}", json!({"stage": "key_down_sent", "key": "F8"}));
  expect_event(
    &events,
    "key_down",
    |event| matches!(event, Event::Key { code, down: true } if *code == usize::from(VK_F8.0)),
  )?;
  drop(input);
  eprintln!("{}", json!({"stage": "input_eof_sent"}));
  expect_event(
    &events,
    "key_up_after_eof",
    |event| matches!(event, Event::Key { code, down: false } if *code == usize::from(VK_F8.0)),
  )?;
  restore.held_f8 = false;
  Ok(json!({
    "display": format!("windows-{number}"), "frame_size": size, "frames_read": frame_count,
    "fixture_colors_verified": true, "capture_eof_verified": true,
    "click_viewport_sizes": viewports, "keyboard_eof_release_verified": true,
  }))
}

pub fn run() -> Result<()> {
  let mut args = std::env::args_os().skip(1);
  const USAGE: &str = "usage: windows-session-check AGENT_EXECUTABLE windows-N";
  let agent = args.next().ok_or(USAGE)?;
  let target = args.next().ok_or(USAGE)?;
  let Target::Windows(selected_number) = Target::parse(target.to_str().ok_or(USAGE)?)? else {
    return Err("the fixture requires an explicit windows-N display target".into());
  };
  if args.next().is_some() {
    return Err(USAGE.into());
  }
  let _physical = PhysicalPixels::enter()?;
  let children = Children::default();
  let watchdog_children = Arc::clone(&children);
  let (_watchdog_done, watchdog_wait) = mpsc::channel::<()>();
  thread::spawn(move || {
    if matches!(
      watchdog_wait.recv_timeout(Duration::from_secs(25)),
      Err(mpsc::RecvTimeoutError::Timeout)
    ) {
      cancel_children(&watchdog_children);
      thread::sleep(Duration::from_millis(1200));
      std::process::exit(124);
    }
  });
  let client = Client::new(CheckedLocal {
    local: Local {
      agent: PathBuf::from(agent),
    },
    children: Arc::clone(&children),
  });
  let displays = client.discover()?;
  let (number, bounds) = displays
    .iter()
    .find_map(|session| match session.display {
      Display::Windows { number, bounds, .. }
        if number == selected_number && session.selectable
          && bounds[2] - bounds[0] >= 480 && bounds[3] - bounds[1] >= 360 =>
      {
        Some((number, bounds))
      }
      _ => None,
    })
    .ok_or_else(|| format!("selected display windows-{selected_number} is not selectable or is too small for the fixture"))?;
  let (events, received) = mpsc::channel();
  let fixture = Fixture::create(bounds, events)?;
  let hwnd_value = fixture.hwnd.0 as usize;
  let (finished, result) = mpsc::channel();
  let worker = thread::spawn(move || {
    let result =
      exercise(client, number, bounds, hwnd_value, received).map_err(|error| error.to_string());
    let _ = finished.send(result);
  });
  let deadline = Instant::now() + Duration::from_secs(20);
  let outcome = loop {
    if let Ok(result) = result.try_recv() {
      break result;
    }
    if Instant::now() >= deadline {
      break Err("Windows session check exceeded 20 seconds".into());
    }
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
      if message.message == WM_QUIT {
        break;
      }
      let _ = unsafe { TranslateMessage(&message) };
      unsafe { DispatchMessageW(&message) };
    }
    thread::sleep(Duration::from_millis(5));
  };
  cancel_children(&children);
  drop(fixture);
  let _ = worker.join();
  // Give the transport's one-second cancellation grace period time to reap any failed child.
  thread::sleep(Duration::from_millis(1100));
  let report = outcome.map_err(io::Error::other)?;
  serde_json::to_writer_pretty(io::stdout().lock(), &report)?;
  println!();
  Ok(())
}
