use ctream_agent::protocol::{Request, TargetOperation, parse};
use ctream_agent::{
  capture as catalog,
  runtime::{
    capture_display, capture_window, forward_input, forward_window_input, ping, video_capabilities,
  },
};
use ctream_driver::{CaptureDisplay, CaptureWindow, ControlSource, Driver, Result};
use ctream_driver_capture::linux as capture;
use ctream_driver_input::linux as input;
use ctream_proto::{Target, VideoRequest};
use std::{env, io};

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Ping => ping(),
    Request::VideoCapabilities => video_capabilities(io::stdout().lock()),
    Request::CaptureCatalog { .. } => {
      serde_json::to_writer(io::stdout().lock(), &catalog::catalog())?;
      Ok(())
    }
    Request::CaptureDrivers(target) => {
      check_capture_target(target)?;
      serde_json::to_writer(io::stdout().lock(), &catalog::list(target))?;
      Ok(())
    }
    Request::CaptureWithDriver {
      target,
      driver,
      window,
    } => capture_selected(target, driver, window, None),
    Request::CaptureVideo {
      target,
      driver,
      window,
      request,
    } => capture_selected(target, driver, window, Some(request)),
    Request::Discover { .. } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match (target, operation) {
      (Target::Windows(_) | Target::WindowsDesktop, _) => {
        Err("Windows targets require a native Windows agent".into())
      }
      (Target::MacosDesktop, _) => Err("macOS targets require a native macOS agent".into()),
      (Target::X11(display), TargetOperation::InputWindow(id)) => {
        forward_window_input(input::x11::Driver, &display, id)
      }
      (Target::Wayland(_), TargetOperation::InputWindow(_)) => {
        Err("pointer and keyboard input are unavailable for Wayland windows".into())
      }
      (target, TargetOperation::Windows) => {
        serde_json::to_writer(
          io::stdout().lock(),
          &ctream_platform::windows::list(target)?,
        )?;
        Ok(())
      }
      (Target::X11(display), TargetOperation::Capture) => {
        capture_display(capture::x11::ffmpeg::Driver, &display)
      }
      (Target::Wayland(display), TargetOperation::Capture) => {
        capture_display(capture::wayland::grim::Driver, &display)
      }
      (Target::X11(display), TargetOperation::CaptureWindow(id)) => {
        capture_window(capture::x11::composite::Driver, &display, id)
      }
      (Target::Wayland(display), TargetOperation::CaptureWindow(id)) => {
        capture_window(capture::wayland::image_copy::Driver, &display, id)
      }
      (Target::X11(display), TargetOperation::Input) => forward_input(input::x11::Driver, &display),
      (Target::Wayland(display), TargetOperation::Input) => {
        forward_input(input::wayland::Driver, &display)
      }
    },
    Request::Controls(target, id, operation) => {
      ctream_platform::controls::resolve(target, id)?.run(operation)
    }
  }
}

fn check_capture_target(target: Target) -> Result<()> {
  match target {
    Target::Windows(_) | Target::WindowsDesktop => {
      Err("Windows capture drivers require a native Windows agent".into())
    }
    Target::MacosDesktop => Err("macOS capture drivers require a native macOS agent".into()),
    _ => Ok(()),
  }
}

fn capture_selected(
  target: Target,
  driver: &str,
  window: Option<&str>,
  video: Option<VideoRequest>,
) -> Result<()> {
  check_capture_target(target)?;
  let selected = catalog::select(target, driver, window.is_some())?;
  match (target, selected.id, window) {
    (Target::X11(display), "linux.x11.ffmpeg", None) => selected.stream(
      capture::x11::ffmpeg::Driver
        .open(&display)?
        .capture_display()?,
      video,
    ),
    (Target::X11(display), "linux.x11.composite", Some(id)) => selected.stream(
      capture::x11::composite::Driver
        .open(&display)?
        .capture_window(id)?,
      video,
    ),
    (Target::Wayland(display), "linux.wayland.grim", None) => selected.stream(
      capture::wayland::grim::Driver
        .open(&display)?
        .capture_display()?,
      video,
    ),
    (Target::Wayland(display), "linux.wayland.image_copy", Some(id)) => selected.stream(
      capture::wayland::image_copy::Driver
        .open(&display)?
        .capture_window(id)?,
      video,
    ),
    _ => Err("capture driver is unavailable for this target".into()),
  }
}
