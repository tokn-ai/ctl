use ctream_agent::{
  protocol::{Request, TargetOperation, parse},
  runtime::{capture_display, capture_window, forward_input, forward_window_input},
};
use ctream_driver::Result;
use ctream_driver_capture::windows as capture;
use ctream_driver_input::windows as input;
use ctream_proto::Target;
use std::{env, io};

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Discover {
      windows_displays: true,
    } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Discover {
      windows_displays: false,
    } => Err("Windows display discovery requires ctream_agent 1.0.5".into()),
    Request::Target(Target::Windows(display), TargetOperation::Capture) => {
      capture_display(capture::Driver, &display)
    }
    Request::Target(Target::Windows(display), TargetOperation::Input) => {
      forward_input(input::Driver, &display)
    }
    Request::Target(target @ Target::Windows(_), TargetOperation::Windows) => {
      serde_json::to_writer(
        io::stdout().lock(),
        &ctream_platform::windows::list(target)?,
      )?;
      Ok(())
    }
    Request::Target(Target::Windows(display), TargetOperation::CaptureWindow(id)) => {
      capture_window(capture::Driver, &display, id)
    }
    Request::Target(Target::Windows(display), TargetOperation::InputWindow(id)) => {
      forward_window_input(input::Driver, &display, id)
    }
    _ => Err("this agent supports Windows display and window capture/input".into()),
  }
}
