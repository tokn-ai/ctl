use super::{
  protocol::{Key, Topology, absolute_position},
  state::{self, Backend},
  window::{self, Capture, Routing, WindowBackend},
};
use ctream_driver::{ForwardInput, ForwardWindowInput, InputSource, Result};
use ctream_windows::WindowTarget;
use std::{
  io,
  marker::PhantomData,
  mem::size_of,
  rc::Rc,
  thread,
  time::{Duration, Instant},
};
use windows::{
  Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT, SetLastError, WIN32_ERROR},
    Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW},
    UI::{
      HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
      },
      Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
        MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
      },
      WindowsAndMessaging::{
        GA_ROOT, GUITHREADINFO, GetAncestor, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
        SetForegroundWindow, WindowFromPoint,
      },
    },
  },
  core::{BOOL, Error},
};

/// Windows SendInput driver selecting the number in the GDI DISPLAYn name.
pub struct Driver;
pub struct Session {
  display: u32,
}
pub struct Input {
  display: u32,
}
pub struct WindowInput {
  target: WindowTarget,
}

impl WindowInput {
  pub fn open_desktop(id: &str) -> Result<Self> {
    Ok(Self {
      target: WindowTarget::open_desktop(id)?,
    })
  }
}

impl ctream_driver::Driver for Driver {
  type Target = u32;
  type Session = Session;

  fn open(&self, display: &u32) -> Result<Session> {
    let _dpi = Dpi::enter()?;
    monitors(*display)?;
    Ok(Session { display: *display })
  }
}

impl ForwardInput for Session {
  type Input = Input;

  fn input(&self) -> Result<Input> {
    Ok(Input {
      display: self.display,
    })
  }
}

impl ForwardWindowInput for Session {
  type Input = WindowInput;

  fn input_window(&self, id: &str) -> Result<WindowInput> {
    Ok(WindowInput {
      target: WindowTarget::open(self.display, id)?,
    })
  }
}

impl InputSource for WindowInput {
  fn run(self) -> Result<()> {
    let _dpi = Dpi::enter()?;
    self.target.validate()?;
    state::run_monitored(
      window::Window::new(NativeWindow {
        target: self.target,
      }),
      io::BufReader::new(io::stdin()),
      &mut io::stdout().lock(),
    )?;
    Ok(())
  }
}

impl InputSource for Input {
  fn run(self) -> Result<()> {
    // Keep the DPI guard on this execution thread throughout monitor queries.
    let _dpi = Dpi::enter()?;
    let topology = monitors(self.display)?;
    state::run(
      Native {
        display: self.display,
        topology,
      },
      &mut io::stdin().lock(),
      &mut io::stdout().lock(),
    )?;
    Ok(())
  }
}

struct Dpi {
  previous: DPI_AWARENESS_CONTEXT,
  _thread: PhantomData<Rc<()>>,
}

impl Dpi {
  fn enter() -> io::Result<Self> {
    let previous =
      unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
      return Err(io::Error::other(Error::from_thread()));
    }
    Ok(Self {
      previous,
      _thread: PhantomData,
    })
  }
}

impl Drop for Dpi {
  fn drop(&mut self) {
    unsafe { SetThreadDpiAwarenessContext(self.previous) };
  }
}

struct Monitors {
  name: Vec<u16>,
  selected: Option<[i32; 4]>,
  selected_token: usize,
  desktop: Option<[i32; 4]>,
  error: Option<Error>,
}

unsafe extern "system" fn monitor_callback(
  monitor: HMONITOR,
  _: HDC,
  _: *mut RECT,
  data: LPARAM,
) -> BOOL {
  // EnumDisplayMonitors invokes this synchronously with the live stack value.
  let query = unsafe { &mut *(data.0 as *mut Monitors) };
  let mut info = MONITORINFOEXW::default();
  info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
  if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
    query.error = Some(Error::from_thread());
    return BOOL(0);
  }
  let RECT {
    left,
    top,
    right,
    bottom,
  } = info.monitorInfo.rcMonitor;
  if right <= left || bottom <= top {
    return BOOL(1);
  }
  let rectangle = [left, top, right, bottom];
  let length = info
    .szDevice
    .iter()
    .position(|unit| *unit == 0)
    .unwrap_or(info.szDevice.len());
  if info.szDevice[..length] == query.name {
    query.selected = Some(rectangle);
    query.selected_token = monitor.0 as usize;
  }
  query.desktop = Some(match query.desktop {
    None => rectangle,
    Some([x, y, r, b]) => [x.min(left), y.min(top), r.max(right), b.max(bottom)],
  });
  BOOL(1)
}

fn monitors(display: u32) -> io::Result<Topology> {
  if display == 0 {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Windows display numbers start at 1",
    ));
  }
  let query = query_monitors(format!(r"\\.\DISPLAY{display}").encode_utf16().collect())?;
  match (query.selected, query.desktop) {
    (Some(monitor), Some(desktop)) => Ok(Topology {
      monitor_token: query.selected_token,
      monitor,
      desktop,
    }),
    _ => Err(io::Error::new(
      io::ErrorKind::NotFound,
      "selected Windows display is not attached to this desktop",
    )),
  }
}

fn query_monitors(name: Vec<u16>) -> io::Result<Monitors> {
  let mut query = Monitors {
    name,
    selected: None,
    selected_token: 0,
    desktop: None,
    error: None,
  };
  let result = unsafe {
    EnumDisplayMonitors(
      None,
      None,
      Some(monitor_callback),
      LPARAM((&mut query as *mut Monitors) as isize),
    )
  };
  if let Some(error) = query.error {
    return Err(io::Error::other(error));
  }
  if !result.as_bool() {
    return Err(io::Error::other(Error::from_thread()));
  }
  Ok(query)
}

fn send(input: INPUT) -> io::Result<()> {
  // UIPI blocking does not necessarily set an error; do not report a stale one.
  unsafe { SetLastError(WIN32_ERROR(0)) };
  if unsafe { SendInput(&[input], size_of::<INPUT>() as i32) } != 1 {
    let error = Error::from_thread();
    return Err(io::Error::other(format!(
      "SendInput rejected an event (HRESULT 0x{:08X}); desktop access or integrity level may block input",
      error.code().0 as u32
    )));
  }
  Ok(())
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: u32, position: [i32; 2]) -> io::Result<()> {
  send(INPUT {
    r#type: INPUT_MOUSE,
    Anonymous: INPUT_0 {
      mi: MOUSEINPUT {
        dx: position[0],
        dy: position[1],
        mouseData: data,
        dwFlags: flags,
        ..Default::default()
      },
    },
  })
}

struct Native {
  display: u32,
  topology: Topology,
}

impl Backend for Native {
  fn validate_target(&mut self) -> io::Result<()> {
    let current = monitors(self.display)?;
    self.topology.require_same(current)
  }

  fn move_pointer(&mut self, point: [u32; 2], size: [u32; 2]) -> io::Result<()> {
    mouse(
      MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
      0,
      absolute_position(point, size, self.topology.monitor, self.topology.desktop)?,
    )
  }

  fn key(&mut self, key: Key, pressed: bool) -> io::Result<()> {
    inject_key(key, pressed)
  }

  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    inject_button(code, pressed)
  }
}

fn inject_key(key: Key, pressed: bool) -> io::Result<()> {
  let (virtual_key, scan, mut flags) = match key {
    Key::Scan { code, extended } => (
      0,
      code,
      KEYEVENTF_SCANCODE
        | if extended {
          KEYEVENTF_EXTENDEDKEY
        } else {
          KEYBD_EVENT_FLAGS(0)
        },
    ),
    Key::Virtual(code) => (code, 0, KEYBD_EVENT_FLAGS(0)),
  };
  if !pressed {
    flags |= KEYEVENTF_KEYUP;
  }
  send(INPUT {
    r#type: INPUT_KEYBOARD,
    Anonymous: INPUT_0 {
      ki: KEYBDINPUT {
        wVk: VIRTUAL_KEY(virtual_key),
        wScan: scan,
        dwFlags: flags,
        ..Default::default()
      },
    },
  })
}

fn inject_button(code: u8, pressed: bool) -> io::Result<()> {
  let (flags, data) = match code {
    1 => (
      if pressed {
        MOUSEEVENTF_LEFTDOWN
      } else {
        MOUSEEVENTF_LEFTUP
      },
      0,
    ),
    2 => (
      if pressed {
        MOUSEEVENTF_MIDDLEDOWN
      } else {
        MOUSEEVENTF_MIDDLEUP
      },
      0,
    ),
    3 => (
      if pressed {
        MOUSEEVENTF_RIGHTDOWN
      } else {
        MOUSEEVENTF_RIGHTUP
      },
      0,
    ),
    4 => (MOUSEEVENTF_WHEEL, 120),
    5 => (MOUSEEVENTF_WHEEL, (-120_i32) as u32),
    6 => (MOUSEEVENTF_HWHEEL, (-120_i32) as u32),
    7 => (MOUSEEVENTF_HWHEEL, 120),
    8 | 9 => (
      if pressed {
        MOUSEEVENTF_XDOWN
      } else {
        MOUSEEVENTF_XUP
      },
      u32::from(code - 7),
    ),
    _ => {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "unknown button",
      ));
    }
  };
  mouse(flags, data, [0, 0])
}

struct NativeWindow {
  target: WindowTarget,
}

fn root(hwnd: HWND) -> HWND {
  if hwnd.0.is_null() {
    hwnd
  } else {
    unsafe { GetAncestor(hwnd, GA_ROOT) }
  }
}

impl WindowBackend for NativeWindow {
  fn validate(&mut self) -> io::Result<()> {
    self
      .target
      .validate()
      .map_err(|error| io::Error::other(error.to_string()))
  }

  fn geometry(&mut self) -> io::Result<([i32; 4], [i32; 4])> {
    let bounds = self
      .target
      .bounds()
      .map_err(|error| io::Error::other(error.to_string()))?;
    // A window can move across the virtual desktop after either kind of open.
    let desktop = query_monitors(Vec::new())?.desktop.ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "Windows desktop has no attached displays",
      )
    })?;
    Ok((bounds, desktop))
  }

  fn routing(&mut self) -> io::Result<Routing> {
    let target = self.target.hwnd();
    let foreground = unsafe { GetForegroundWindow() };
    let mut info = GUITHREADINFO {
      cbSize: size_of::<GUITHREADINFO>() as u32,
      ..Default::default()
    };
    // Query even a foreign foreground thread: its capture can redirect a click
    // away from an otherwise visible selected window.
    unsafe { GetGUIThreadInfo(0, &mut info) }.map_err(io::Error::other)?;
    if unsafe { GetForegroundWindow() } != foreground {
      return Err(io::Error::other(
        "foreground window changed while checking input routing; input was not sent",
      ));
    }
    Ok(Routing {
      foreground: root(info.hwndActive) == target && root(foreground) == target,
      focus: root(info.hwndFocus) == target,
      capture: if info.hwndCapture.0.is_null() {
        Capture::None
      } else if root(info.hwndCapture) == target {
        Capture::Target
      } else {
        Capture::Other
      },
    })
  }

  fn activate(&mut self) -> io::Result<()> {
    self.validate()?;
    if !unsafe { SetForegroundWindow(self.target.hwnd()) }.as_bool() {
      return Err(io::Error::other(
        "Windows denied activation of the selected window; click a visible part of it before scrolling or typing",
      ));
    }
    Ok(())
  }

  fn hit_target(&mut self, [x, y]: [i32; 2]) -> io::Result<bool> {
    Ok(root(unsafe { WindowFromPoint(POINT { x, y }) }) == self.target.hwnd())
  }

  fn move_pointer(&mut self, absolute: [i32; 2]) -> io::Result<()> {
    mouse(
      MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
      0,
      absolute,
    )
  }

  fn wait_pointer(&mut self, physical: [i32; 2]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(50);
    loop {
      let mut point = POINT::default();
      unsafe { GetCursorPos(&mut point) }.map_err(io::Error::other)?;
      if [point.x, point.y] == physical {
        return Ok(());
      }
      if Instant::now() >= deadline {
        return Err(io::Error::other(
          "cursor did not reach the selected window point; input was not sent",
        ));
      }
      thread::sleep(Duration::from_millis(1));
    }
  }

  fn key(&mut self, key: Key, pressed: bool) -> io::Result<()> {
    inject_key(key, pressed)
  }

  fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    inject_button(code, pressed)
  }
}
