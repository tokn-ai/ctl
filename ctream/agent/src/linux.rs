use ctream_agent::protocol::{Request, TargetOperation, parse};
use ctream_agent::runtime::{capture_display, capture_window, forward_input};
use ctream_driver::{ControlSource, Result};
use ctream_driver_capture::linux as capture;
use ctream_driver_input::linux as input;
use ctream_proto::Target;
use std::{env, io};

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Discover { .. } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match (target, operation) {
      (Target::Windows(_), _) => Err("Windows displays require a native Windows agent".into()),
      (target, TargetOperation::Windows) => {
        serde_json::to_writer(
          io::stdout().lock(),
          &ctream_platform::windows::list(target)?,
        )?;
        Ok(())
      }
      (Target::X11(display), TargetOperation::Capture) => {
        capture_display(capture::x11::Driver, &display)
      }
      (Target::Wayland(display), TargetOperation::Capture) => {
        capture_display(capture::wayland::Driver, &display)
      }
      (Target::X11(display), TargetOperation::CaptureWindow(id)) => {
        capture_window(capture::x11::Driver, &display, id)
      }
      (Target::Wayland(display), TargetOperation::CaptureWindow(id)) => {
        capture_window(capture::wayland::Driver, &display, id)
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
