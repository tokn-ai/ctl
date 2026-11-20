//! Windows display capture through DXGI and window capture through WGC.
//!
//! Frames are RGB PPM with physical dimensions. DXGI display capture omits the
//! separate hardware cursor; WGC window capture retains the OS cursor policy.

mod capture;
mod dpi;
mod id;
mod pixels;
mod readback;
mod window;

pub use capture::DxgiCapture;
pub use window::WgcCapture;

use ctream_driver::{CaptureDisplay, CaptureWindow, DiscoverWindows, Result};
use dpi::PhysicalPixels;
use id::display_number;
use std::collections::BTreeMap;
use windows::Win32::Graphics::Dxgi::{
  CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_OUTPUT_DESC, IDXGIAdapter1, IDXGIFactory1,
  IDXGIOutput,
};

/// A DXGI output identified by the number in its Windows GDI device name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayInfo {
  pub number: u32,
  pub name: String,
  /// Physical desktop coordinates: left, top, right, bottom.
  pub bounds: [i32; 4],
}

struct Output {
  adapter: IDXGIAdapter1,
  output: IDXGIOutput,
  description: DXGI_OUTPUT_DESC,
  info: DisplayInfo,
}

fn outputs() -> Result<Vec<Output>> {
  let _physical = PhysicalPixels::enter()?;
  let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }?;
  let mut outputs = BTreeMap::new();
  for adapter_index in 0..32 {
    let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
      Ok(adapter) => adapter,
      Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
        return Ok(outputs.into_values().collect());
      }
      Err(error) => return Err(error.into()),
    };
    let mut complete = false;
    for output_index in 0..32 {
      let output = match unsafe { adapter.EnumOutputs(output_index) } {
        Ok(output) => output,
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => {
          complete = true;
          break;
        }
        Err(error) => {
          return Err(format!("Windows display enumeration failed; launch the agent in an interactive desktop session: {error}").into());
        }
      };
      let description = unsafe { output.GetDesc() }?;
      if !description.AttachedToDesktop.as_bool() {
        continue;
      }
      let name_end = description
        .DeviceName
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(description.DeviceName.len());
      let name = String::from_utf16(&description.DeviceName[..name_end])?;
      let number = display_number(&name).ok_or("unsupported Windows display identifier")?;
      let rect = description.DesktopCoordinates;
      if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err("invalid Windows display geometry".into());
      }
      let info = DisplayInfo {
        number,
        name,
        bounds: [rect.left, rect.top, rect.right, rect.bottom],
      };
      let item = Output {
        adapter: adapter.clone(),
        output,
        description,
        info,
      };
      if outputs.insert(number, item).is_some() {
        return Err("duplicate Windows display identifier".into());
      }
    }
    if !complete {
      return Err("Windows display enumeration exceeded the output limit".into());
    }
  }
  Err("Windows display enumeration exceeded the adapter limit".into())
}

/// Enumerate attached displays without allocating a capture device or duplicator.
pub fn discover() -> Result<Vec<DisplayInfo>> {
  Ok(outputs()?.into_iter().map(|output| output.info).collect())
}

pub struct Driver;

pub struct Session {
  output: Output,
}

impl ctream_driver::Driver for Driver {
  type Target = u32;
  type Session = Session;

  fn open(&self, number: &u32) -> Result<Session> {
    let output = outputs()?
      .into_iter()
      .find(|output| output.info.number == *number)
      .ok_or_else(|| format!("Windows display windows-{number} is not attached"))?;
    Ok(Session { output })
  }
}

impl CaptureDisplay for Session {
  type Source = DxgiCapture;

  fn capture_display(&self) -> Result<DxgiCapture> {
    DxgiCapture::new(&self.output)
  }
}

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<ctream_proto::Window>> {
    ctream_windows::list_windows(self.output.info.number)
  }
}

impl CaptureWindow for Session {
  type Source = WgcCapture;

  fn capture_window(&self, id: &str) -> Result<WgcCapture> {
    WgcCapture::new(&self.output, id)
  }
}
