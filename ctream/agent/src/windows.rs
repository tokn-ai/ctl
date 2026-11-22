use ctream_agent::{
  capture as catalog,
  protocol::{Request, TargetOperation, parse},
  runtime::{
    capture_display, capture_with_metadata, forward_input, forward_window_input, ping,
    stream_capture,
  },
};
use ctream_driver::{CaptureDisplay, CaptureWindow, Driver, InputSource, Result};
use ctream_driver_capture::windows as capture;
use ctream_driver_input::windows as input;
use ctream_proto::Target;
use std::{env, io};

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Ping => ping(),
    Request::CaptureCatalog { .. } => {
      serde_json::to_writer(io::stdout().lock(), &catalog::catalog())?;
      Ok(())
    }
    Request::CaptureDrivers(target @ (Target::Windows(_) | Target::WindowsDesktop)) => {
      serde_json::to_writer(io::stdout().lock(), &catalog::list(target))?;
      Ok(())
    }
    Request::CaptureWithDriver {
      target: target @ (Target::Windows(_) | Target::WindowsDesktop),
      driver,
      window,
    } => {
      let selected = catalog::select(target, driver, window.is_some())?;
      match (target, selected.id, window) {
        (Target::Windows(display), "windows.dxgi", None) => capture_with_metadata(
          capture::dxgi::Driver.open(&display)?.capture_display()?,
          selected.id,
          selected.format,
        ),
        (_, "windows.wgc", Some(id)) => {
          capture_with_metadata(open_window(target, id)?, selected.id, selected.format)
        }
        _ => Err("capture driver is unavailable for this target".into()),
      }
    }
    Request::Discover {
      windows_displays: true,
      ..
    } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Discover {
      windows_displays: false,
      ..
    } => Err("Windows display discovery requires ctream_agent 1.0.5".into()),
    Request::Target(Target::Windows(display), TargetOperation::Capture) => {
      capture_display(capture::dxgi::Driver, &display)
    }
    Request::Target(Target::Windows(display), TargetOperation::Input) => {
      forward_input(input::Driver, &display)
    }
    Request::Target(
      target @ (Target::Windows(_) | Target::WindowsDesktop),
      TargetOperation::Windows,
    ) => {
      serde_json::to_writer(
        io::stdout().lock(),
        &ctream_platform::windows::list(target)?,
      )?;
      Ok(())
    }
    Request::Target(
      target @ (Target::Windows(_) | Target::WindowsDesktop),
      TargetOperation::CaptureWindow(id),
    ) => stream_capture(open_window(target, id)?),
    Request::Target(Target::Windows(display), TargetOperation::InputWindow(id)) => {
      forward_window_input(input::Driver, &display, id)
    }
    Request::Target(Target::WindowsDesktop, TargetOperation::InputWindow(id)) => {
      input::WindowInput::open_desktop(id)?.run()
    }
    _ => Err("this agent supports Windows display and window capture/input".into()),
  }
}

fn open_window(target: Target, id: &str) -> Result<capture::wgc::WgcCapture> {
  match target {
    Target::Windows(display) => capture::wgc::Driver.open(&display)?.capture_window(id),
    Target::WindowsDesktop => capture::wgc::WgcCapture::open_desktop(id),
    _ => Err("Windows Graphics Capture requires a Windows window".into()),
  }
}
