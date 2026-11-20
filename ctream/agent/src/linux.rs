use ctream_agent::protocol::{Request, TargetOperation, parse};
use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, ControlSource, Driver,
  ForwardInput, InputSource, Result,
};
use ctream_driver_capture::linux as capture;
use ctream_driver_input::linux as input;
use ctream_proto::Target;
use std::{env, io};

fn capture_display<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: CaptureDisplay,
{
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  driver
    .open(target)?
    .capture_display()?
    .run(&mut io::stdout(), &lifetime)
}

fn capture_window<D: Driver>(driver: D, target: &D::Target, id: &str) -> Result<()>
where
  D::Session: CaptureWindow,
{
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  driver
    .open(target)?
    .capture_window(id)?
    .run(&mut io::stdout(), &lifetime)
}

fn forward_input<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: ForwardInput,
{
  driver.open(target)?.input()?.run()
}

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Discover => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match (target, operation) {
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
