use ctream_agent::native_input::{self, Gate, Mode};
use ctream_agent::{
  capture as catalog,
  protocol::{Request, TargetOperation, parse},
  runtime::{input_capabilities, ping, video_capabilities},
};
use ctream_driver::{Driver, ForwardInput, ForwardWindowInput, InputSource, Result};
use ctream_driver_input::windows as input;
use ctream_proto::Target;
use std::io;

pub(super) fn run(args: &[String]) -> Result<()> {
  match parse(args)? {
    Request::ViewerLane(target, window, lane) => crate::viewer::run(target, window, lane),
    Request::LiveRoom {
      turns,
      authoritative,
      target,
      agent_id,
      window,
    } => crate::live_room(target, agent_id, window, turns, authoritative),
    Request::AttributedSession {
      protocol,
      target,
      agent_id,
      window,
    } => crate::attributed::serve(target, agent_id, window, protocol),
    Request::SessionCapture { .. }
    | Request::SessionInput { .. }
    | Request::ViewerCapture { .. }
    | Request::LeasedViewerInput { .. } => {
      Err("window capture leases require a macOS agent".into())
    }
    Request::Ping => ping(),
    Request::InputCapabilities => input_capabilities(io::stdout().lock()),
    Request::ViewerInput {
      target,
      label,
      window,
      receipts,
    } => raw_input(target, window, Some(label), Mode::Viewer { receipts }),
    Request::InputSync(target, window) => raw_input(target, window, None, Mode::Synchronized),
    Request::Target(target, TargetOperation::InputWindow(window)) => {
      raw_input(target, Some(window), None, Mode::Legacy)
    }
    Request::Target(target, TargetOperation::Input) => raw_input(target, None, None, Mode::Legacy),
    Request::VideoCapabilities => video_capabilities(io::stdout().lock()),
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
    } => ctream_agent::daemon::capture_stream(target, driver, window, None, true),
    Request::CaptureVideo {
      target: target @ (Target::Windows(_) | Target::WindowsDesktop),
      driver,
      window,
      request,
    } => ctream_agent::daemon::capture_stream(target, driver, window, Some(request), true),
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
      ctream_agent::daemon::capture_stream(Target::Windows(display), "auto", None, None, false)
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
    ) => ctream_agent::daemon::capture_stream(target, "auto", Some(id), None, false),
    _ => Err("this agent supports Windows display and window capture/input".into()),
  }
}

fn raw_input(target: Target, window: Option<&str>, label: Option<&str>, mode: Mode) -> Result<()> {
  let Ok(key) = std::env::var("CTREAM_INPUT_DELEGATION") else {
    let authority = native_input::controller(target, window, label, None)?;
    return Ok(native_input::serve_controller(
      mode,
      authority,
      None,
      None,
      io::BufReader::new(io::stdin().lock()),
      &mut io::stdout().lock(),
    )?);
  };
  let gate = Gate::delegated(target, window, &key)?;
  native_input::serve_driver(mode, gate, move |reader, writer| match (target, window) {
    (Target::Windows(display), None) => input::Driver
      .open(&display)?
      .input()?
      .run_piped(reader, writer),
    (Target::Windows(display), Some(id)) => input::Driver
      .open(&display)?
      .input_window(id)?
      .run_piped(reader, writer),
    (Target::WindowsDesktop, Some(id)) => {
      input::WindowInput::open_desktop(id)?.run_piped(reader, writer)
    }
    _ => Err("this agent supports Windows displays and windows".into()),
  })
}
