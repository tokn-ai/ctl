use ctream_agent::protocol::{Request, TargetOperation, parse};
use ctream_agent::{
  capture as catalog,
  runtime::{capture_display, capture_window, capture_with_metadata, forward_input},
};
use ctream_driver::{CaptureDisplay, CaptureWindow, ControlSource, Driver, Result};
use ctream_driver_capture::linux as capture;
use ctream_driver_input::linux as input;
use ctream_proto::Target;
use std::{env, io};

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::CaptureCatalog => {
      serde_json::to_writer(io::stdout().lock(), &catalog::catalog())?;
      Ok(())
    }
    Request::CaptureDrivers(Target::Windows(_))
    | Request::CaptureWithDriver {
      target: Target::Windows(_),
      ..
    } => Err("Windows capture drivers require a native Windows agent".into()),
    Request::CaptureDrivers(target) => {
      serde_json::to_writer(io::stdout().lock(), &catalog::list(target))?;
      Ok(())
    }
    Request::CaptureWithDriver {
      target,
      driver,
      window,
    } => {
      let selected = catalog::select(target, driver, window.is_some())?;
      match (target, selected.id, window) {
        (Target::X11(display), "linux.x11.ffmpeg", None) => capture_with_metadata(
          capture::x11::ffmpeg::Driver
            .open(&display)?
            .capture_display()?,
          selected.id,
          selected.format,
        ),
        (Target::X11(display), "linux.x11.composite", Some(id)) => capture_with_metadata(
          capture::x11::composite::Driver
            .open(&display)?
            .capture_window(id)?,
          selected.id,
          selected.format,
        ),
        (Target::Wayland(display), "linux.wayland.grim", None) => capture_with_metadata(
          capture::wayland::grim::Driver
            .open(&display)?
            .capture_display()?,
          selected.id,
          selected.format,
        ),
        (Target::Wayland(display), "linux.wayland.image_copy", Some(id)) => capture_with_metadata(
          capture::wayland::image_copy::Driver
            .open(&display)?
            .capture_window(id)?,
          selected.id,
          selected.format,
        ),
        _ => Err("capture driver is unavailable for this target".into()),
      }
    }
    Request::Discover { .. } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match (target, operation) {
      (Target::Windows(_), _) => Err("Windows displays require a native Windows agent".into()),
      (_, TargetOperation::InputWindow(_)) => Err("window input requires Windows".into()),
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
