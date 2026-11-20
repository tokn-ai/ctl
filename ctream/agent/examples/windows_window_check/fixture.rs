use std::{
  error::Error,
  sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
  },
};
use windows::{
  Win32::{
    Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::{
      Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
      Gdi::{
        BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
        InvalidateRect, PAINTSTRUCT, UpdateWindow,
      },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
      HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
      },
      WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
        GetClientRect, GetForegroundWindow, GetWindowLongPtrW, HWND_TOPMOST, IsWindow, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
        SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, TranslateMessage, WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP,
      },
    },
  },
  core::{Error as WindowsError, PCWSTR, w},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub const CLASS: &str = "CtreamWindowsWindowFixture";
pub const COLORS: [[[u8; 3]; 2]; 2] = [
  [[42, 176, 96], [64, 112, 192]],
  [[224, 136, 40], [128, 64, 192]],
];
const COVER_COLOR: [u8; 3] = [212, 28, 76];

pub struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
  pub fn enter() -> Result<Self> {
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
pub enum Event {
  Click {
    target: bool,
    x: i32,
    y: i32,
    down: bool,
  },
  Key {
    target: bool,
    code: usize,
    down: bool,
  },
  Closed,
}

struct State {
  target: bool,
  events: mpsc::Sender<Event>,
  phase: AtomicBool,
  palette: AtomicUsize,
}

unsafe extern "system" fn window_proc(
  hwnd: HWND,
  message: u32,
  wparam: WPARAM,
  lparam: LPARAM,
) -> LRESULT {
  let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
  if !state.is_null() {
    // The UI thread owns the box until DestroyWindow has returned.
    let state = unsafe { &*state };
    match message {
      WM_TIMER => {
        state.phase.fetch_xor(true, Ordering::Relaxed);
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        let _ = unsafe { UpdateWindow(hwnd) };
        return LRESULT(0);
      }
      WM_PAINT => {
        let mut paint = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut paint) };
        let [r, g, b] = if state.target {
          COLORS[state.palette.load(Ordering::Relaxed)]
            [usize::from(state.phase.load(Ordering::Relaxed))]
        } else {
          COVER_COLOR
        };
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
          target: state.target,
          x: (lparam.0 as u16 as i16).into(),
          y: ((lparam.0 >> 16) as u16 as i16).into(),
          down: message == WM_LBUTTONDOWN,
        });
        return LRESULT(0);
      }
      WM_KEYDOWN | WM_KEYUP => {
        let _ = state.events.send(Event::Key {
          target: state.target,
          code: wparam.0,
          down: message == WM_KEYDOWN,
        });
        return LRESULT(0);
      }
      WM_DESTROY if state.target => {
        let _ = state.events.send(Event::Closed);
      }
      _ => {}
    }
  }
  unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

struct OwnedWindow {
  hwnd: HWND,
  state: Box<State>,
}

impl OwnedWindow {
  fn create(
    bounds: [i32; 4],
    title: &str,
    target: bool,
    events: mpsc::Sender<Event>,
  ) -> Result<Self> {
    let instance = HINSTANCE(unsafe { GetModuleHandleW(None) }?.0);
    let state = Box::new(State {
      target,
      events,
      phase: AtomicBool::new(false),
      palette: AtomicUsize::new(0),
    });
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let hwnd = unsafe {
      CreateWindowExW(
        WS_EX_TOPMOST,
        w!("CtreamWindowsWindowFixture"),
        PCWSTR(title.as_ptr()),
        if target {
          WS_OVERLAPPEDWINDOW
        } else {
          WS_POPUP
        },
        bounds[0] + 80,
        bounds[1] + 80,
        420,
        300,
        None,
        None,
        Some(instance),
        None,
      )
    }?;
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&*state as *const State) as isize) };
    let window = Self { hwnd, state };
    if target {
      if unsafe { SetTimer(Some(hwnd), 1, 200, None) } == 0 {
        return Err(WindowsError::from_thread().into());
      }
      let _ = unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    }
    Ok(window)
  }
}

impl Drop for OwnedWindow {
  fn drop(&mut self) {
    if unsafe { IsWindow(Some(self.hwnd)) }.as_bool() {
      let _ = unsafe { DestroyWindow(self.hwnd) };
    }
  }
}

pub enum Action {
  Resize([i32; 2]),
  Cover(bool),
  FocusTarget,
  FocusCover,
  CloseTarget,
}

pub struct Request {
  pub action: Action,
  pub reply: mpsc::Sender<std::result::Result<(), String>>,
}

pub struct Ui {
  target: OwnedWindow,
  cover: OwnedWindow,
}

impl Ui {
  pub fn create(bounds: [i32; 4], title: &str, events: mpsc::Sender<Event>) -> Result<Self> {
    let class = WNDCLASSW {
      lpfnWndProc: Some(window_proc),
      hInstance: HINSTANCE(unsafe { GetModuleHandleW(None) }?.0),
      lpszClassName: w!("CtreamWindowsWindowFixture"),
      ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
      return Err(WindowsError::from_thread().into());
    }
    Ok(Self {
      target: OwnedWindow::create(bounds, title, true, events.clone())?,
      cover: OwnedWindow::create(bounds, "ctream opaque test cover", false, events)?,
    })
  }

  pub fn handles(&self) -> [usize; 2] {
    [self.target.hwnd.0 as usize, self.cover.hwnd.0 as usize]
  }

  pub fn handle(&self, action: Action) -> Result<()> {
    match action {
      Action::Resize([width, height]) => {
        unsafe {
          SetWindowPos(
            self.target.hwnd,
            None,
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
          )
        }?;
      }
      Action::Cover(true) => {
        let geometry = Geometry::read(self.target.hwnd)?;
        unsafe {
          SetWindowPos(
            self.cover.hwnd,
            Some(HWND_TOPMOST),
            geometry.bounds.left - 12,
            geometry.bounds.top - 12,
            geometry.size[0] as i32 + 24,
            geometry.size[1] as i32 + 24,
            SWP_NOACTIVATE,
          )
        }?;
        let _ = unsafe { ShowWindow(self.cover.hwnd, SW_SHOWNOACTIVATE) };
        // New colors cannot be satisfied by frames queued before the window was covered.
        self.target.state.palette.store(1, Ordering::Relaxed);
      }
      Action::Cover(false) => {
        let _ = unsafe { ShowWindow(self.cover.hwnd, SW_HIDE) };
      }
      Action::FocusTarget | Action::FocusCover => {
        let hwnd = if matches!(action, Action::FocusTarget) {
          self.target.hwnd
        } else {
          self.cover.hwnd
        };
        if !unsafe { SetForegroundWindow(hwnd) }.as_bool()
          || unsafe { GetForegroundWindow() } != hwnd
        {
          return Err("Windows denied focus for the owned test window".into());
        }
      }
      Action::CloseTarget => unsafe { DestroyWindow(self.target.hwnd) }?,
    }
    Ok(())
  }
}

pub fn pump_messages() {
  let mut message = MSG::default();
  while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
    let _ = unsafe { TranslateMessage(&message) };
    unsafe { DispatchMessageW(&message) };
  }
}

pub struct Geometry {
  pub bounds: RECT,
  pub client: RECT,
  pub size: [u32; 2],
  origin: POINT,
}

impl Geometry {
  pub fn read(hwnd: HWND) -> Result<Self> {
    let mut bounds = RECT::default();
    unsafe {
      DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        (&mut bounds as *mut RECT).cast(),
        std::mem::size_of::<RECT>() as u32,
      )
    }?;
    let mut client = RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }?;
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
      return Err(WindowsError::from_thread().into());
    }
    if bounds.right <= bounds.left
      || bounds.bottom <= bounds.top
      || client.right < 16
      || client.bottom < 16
    {
      return Err("fixture has invalid physical bounds".into());
    }
    Ok(Self {
      size: [
        (bounds.right - bounds.left) as u32,
        (bounds.bottom - bounds.top) as u32,
      ],
      bounds,
      client,
      origin,
    })
  }

  pub fn screen(&self, client: POINT) -> POINT {
    POINT {
      x: self.origin.x + client.x,
      y: self.origin.y + client.y,
    }
  }

  pub fn frame_point(&self, client: POINT) -> [u32; 2] {
    let screen = self.screen(client);
    [
      (screen.x - self.bounds.left) as u32,
      (screen.y - self.bounds.top) as u32,
    ]
  }

  pub fn sample(&self) -> POINT {
    POINT {
      x: self.client.right / 2,
      y: self.client.bottom / 2,
    }
  }
}
