use crate::{Result, id::Identity, monitors::monitor_info, watch::WindowWatch};
use ctream_proto::Window;
use windows::{
  Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, HWND, LPARAM, RECT},
    Graphics::{
      Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
      Gdi::{MONITOR_DEFAULTTONULL, MonitorFromWindow},
    },
    System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    UI::{
      HiDpi::{
        DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        SetThreadDpiAwarenessContext,
      },
      WindowsAndMessaging::{
        EnumWindows, GA_ROOT, GetAncestor, GetClassNameW, GetShellWindow, GetWindowTextW,
        GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
      },
    },
  },
  core::BOOL,
};

/// Temporarily request physical coordinates without changing the embedding process policy.
pub struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
  pub fn enter() -> Result<Self> {
    let previous =
      unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
      return Err(windows::core::Error::from_thread().into());
    }
    Ok(Self(previous))
  }
}

impl Drop for PhysicalPixels {
  fn drop(&mut self) {
    // The raw handle keeps this guard on its creating thread.
    unsafe { SetThreadDpiAwarenessContext(self.0) };
  }
}

struct Process(HANDLE);

impl Process {
  fn open(pid: u32) -> Result<Self> {
    Ok(Self(unsafe {
      OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
    }?))
  }

  fn creation(&self) -> Result<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(self.0, &mut created, &mut exited, &mut kernel, &mut user) }?;
    if exited.dwHighDateTime != 0 || exited.dwLowDateTime != 0 {
      return Err("Windows window process exited".into());
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
  }
}

impl Drop for Process {
  fn drop(&mut self) {
    let _ = unsafe { CloseHandle(self.0) };
  }
}

fn visible(hwnd: HWND) -> Result<()> {
  if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || !unsafe { IsWindowVisible(hwnd) }.as_bool() {
    return Err("Windows window closed or is no longer visible".into());
  }
  if unsafe { IsIconic(hwnd) }.as_bool() {
    return Err("Windows window is minimized; restore it and reconnect".into());
  }
  let mut cloaked = 0_u32;
  unsafe {
    DwmGetWindowAttribute(
      hwnd,
      DWMWA_CLOAKED,
      (&mut cloaked as *mut u32).cast(),
      size_of::<u32>() as u32,
    )
  }?;
  if cloaked != 0 {
    return Err("Windows window is cloaked or on an inactive desktop".into());
  }
  Ok(())
}

fn display_of(hwnd: HWND) -> Result<u32> {
  let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
  if monitor.0.is_null() {
    return Err("Windows window is not on an attached display".into());
  }
  Ok(monitor_info(monitor)?.number)
}

/// A window bound to its HWND, process generation, and observed lifetime.
/// Display membership is checked at open; moves and resizes preserve this target.
/// IDs are not lifetime capabilities: a same-process HWND replacement between
/// discovery and opening can share an ID, and destroy notifications are asynchronous.
pub struct WindowTarget {
  identity: Identity,
  process: Process,
  window_thread: u32,
  watch: WindowWatch,
}

impl WindowTarget {
  pub fn open(display: u32, id: &str) -> Result<Self> {
    let _physical = PhysicalPixels::enter()?;
    let identity: Identity = id.parse()?;
    let hwnd = HWND(identity.handle as *mut _);
    let mut pid = 0;
    let window_thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if window_thread == 0 || pid != identity.process {
      return Err("Windows window identity changed; query windows again".into());
    }
    let process = Process::open(pid)?;
    if process.creation()? != identity.created {
      return Err("Windows window process changed; query windows again".into());
    }
    let watch = WindowWatch::new(identity.handle, pid, window_thread)?;
    let target = Self {
      identity,
      process,
      window_thread,
      watch,
    };
    target.validate()?;
    if display_of(hwnd)? != display {
      return Err("Windows window is not on the selected display; query windows again".into());
    }
    if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd || hwnd == unsafe { GetShellWindow() } {
      return Err("Windows target is not an application window".into());
    }
    Ok(target)
  }

  pub fn hwnd(&self) -> HWND {
    HWND(self.identity.handle as *mut _)
  }

  pub fn validate(&self) -> Result<()> {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(self.hwnd(), Some(&mut pid)) };
    if self.watch.dead()
      || thread != self.window_thread
      || pid != self.identity.process
      || self.process.creation()? != self.identity.created
    {
      return Err("Windows window closed or its identity changed; query windows again".into());
    }
    visible(self.hwnd())
  }

  /// Physical desktop coordinates of the DWM surface captured by WGC.
  pub fn bounds(&self) -> Result<[i32; 4]> {
    let _physical = PhysicalPixels::enter()?;
    self.validate()?;
    let mut bounds = RECT::default();
    unsafe {
      DwmGetWindowAttribute(
        self.hwnd(),
        DWMWA_EXTENDED_FRAME_BOUNDS,
        (&mut bounds as *mut RECT).cast(),
        size_of::<RECT>() as u32,
      )
    }?;
    if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
      return Err("invalid Windows window geometry".into());
    }
    Ok([bounds.left, bounds.top, bounds.right, bounds.bottom])
  }
}

unsafe extern "system" fn collect(hwnd: HWND, state: LPARAM) -> BOOL {
  let windows = unsafe { &mut *(state.0 as *mut Vec<HWND>) };
  if windows.len() >= 4096 {
    return BOOL(0);
  }
  windows.push(hwnd);
  BOOL(1)
}

/// Visible application windows assigned once to the monitor with their largest overlap.
pub fn list_windows(display: u32) -> Result<Vec<Window>> {
  crate::display(display)?;
  let _physical = PhysicalPixels::enter()?;
  let mut handles = Vec::<HWND>::new();
  unsafe {
    EnumWindows(
      Some(collect),
      LPARAM((&mut handles as *mut Vec<HWND>) as isize),
    )
  }?;
  let mut result = Vec::new();
  for hwnd in handles {
    if visible(hwnd).is_err()
      || display_of(hwnd).ok() != Some(display)
      || unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd
      || hwnd == unsafe { GetShellWindow() }
    {
      continue;
    }
    let mut title = vec![0; 16 * 1024];
    let length = unsafe { GetWindowTextW(hwnd, &mut title) };
    if length <= 0 {
      continue;
    }
    let title = String::from_utf16_lossy(&title[..length as usize]);
    let mut class = [0; 256];
    let class_length = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    let mut process = 0;
    if unsafe { GetWindowThreadProcessId(hwnd, Some(&mut process)) } == 0 {
      continue;
    }
    let Ok(created) = Process::open(process).and_then(|process| process.creation()) else {
      continue;
    };
    let identity = Identity {
      handle: hwnd.0 as usize,
      process,
      created,
    };
    result.push(Window {
      id: identity.to_string(),
      title,
      app_id: String::from_utf16_lossy(&class[..class_length]),
    });
  }
  result.sort_by(|left, right| left.id.cmp(&right.id));
  Ok(result)
}
