use crate::{PhysicalPixels, Result, display_number};
use std::collections::BTreeMap;
use windows::{
  Win32::{
    Foundation::{LPARAM, RECT},
    Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW},
  },
  core::BOOL,
};

/// An attached Win32 monitor, independent of any capture backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
  pub number: u32,
  pub name: String,
  /// Physical desktop coordinates: left, top, right, bottom.
  pub bounds: [i32; 4],
}

pub(crate) fn monitor_info(monitor: HMONITOR) -> Result<DisplayInfo> {
  let mut info = MONITORINFOEXW::default();
  info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
  unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.ok()?;
  let end = info
    .szDevice
    .iter()
    .position(|unit| *unit == 0)
    .unwrap_or(info.szDevice.len());
  let name = String::from_utf16(&info.szDevice[..end])?;
  let number = display_number(&name).ok_or("unsupported Windows display identifier")?;
  let bounds = info.monitorInfo.rcMonitor;
  if bounds.right <= bounds.left || bounds.bottom <= bounds.top {
    return Err("invalid Windows display geometry".into());
  }
  Ok(DisplayInfo {
    number,
    name,
    bounds: [bounds.left, bounds.top, bounds.right, bounds.bottom],
  })
}

struct Enumeration {
  handles: Vec<HMONITOR>,
  exceeded: bool,
}

unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
  let enumeration = unsafe { &mut *(data.0 as *mut Enumeration) };
  if enumeration.handles.len() == 256 {
    enumeration.exceeded = true;
    return BOOL(0);
  }
  enumeration.handles.push(monitor);
  BOOL(1)
}

/// Discover physical monitor geometry without creating a GPU device or capture session.
pub fn list_displays() -> Result<Vec<DisplayInfo>> {
  let _physical = PhysicalPixels::enter()?;
  let mut enumeration = Enumeration {
    handles: Vec::new(),
    exceeded: false,
  };
  let success = unsafe {
    EnumDisplayMonitors(
      None,
      None,
      Some(collect),
      LPARAM((&mut enumeration as *mut Enumeration) as isize),
    )
  };
  if enumeration.exceeded {
    return Err("Windows display enumeration exceeded the monitor limit".into());
  }
  success.ok()?;
  let mut displays = BTreeMap::new();
  for monitor in enumeration.handles {
    let info = monitor_info(monitor)?;
    if displays.insert(info.number, info).is_some() {
      return Err("duplicate Windows display identifier".into());
    }
  }
  Ok(displays.into_values().collect())
}

/// Resolve a display scope using the native monitor catalog.
pub fn display(number: u32) -> Result<DisplayInfo> {
  if number == 0 {
    return Err("invalid Windows display identifier".into());
  }
  list_displays()?
    .into_iter()
    .find(|display| display.number == number)
    .ok_or_else(|| format!("Windows display windows-{number} is not attached").into())
}
