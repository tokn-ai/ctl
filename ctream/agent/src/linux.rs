use ctream_agent::native_input::{self, Gate, Mode};
use ctream_agent::protocol::{Request, TargetOperation, parse};
use ctream_agent::{
  capture as catalog,
  runtime::{input_capabilities, ping, video_capabilities},
};
use ctream_driver::{ControlSource, Driver, ForwardInput, ForwardWindowInput, InputSource, Result};
use ctream_driver_input::linux as input;
use ctream_proto::Target;
use std::io::{self, Read, Write};

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
    Request::CaptureDrivers(target) => {
      check_capture_target(target)?;
      serde_json::to_writer(io::stdout().lock(), &catalog::list(target))?;
      Ok(())
    }
    Request::CaptureWithDriver {
      target,
      driver,
      window,
    } => ctream_agent::daemon::capture_stream(target, driver, window, None, true),
    Request::CaptureVideo {
      target,
      driver,
      window,
      request,
    } => ctream_agent::daemon::capture_stream(target, driver, window, Some(request), true),
    Request::Discover { .. } => {
      serde_json::to_writer(io::stdout().lock(), &ctream_platform::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match (target, operation) {
      (Target::Windows(_) | Target::WindowsDesktop, _) => {
        Err("Windows targets require a native Windows agent".into())
      }
      (Target::MacosDesktop, _) => Err("macOS targets require a native macOS agent".into()),
      (target, TargetOperation::Windows) => {
        serde_json::to_writer(
          io::stdout().lock(),
          &ctream_platform::windows::list(target)?,
        )?;
        Ok(())
      }
      (target, TargetOperation::Capture) => {
        ctream_agent::daemon::capture_stream(target, "auto", None, None, false)
      }
      (target, TargetOperation::CaptureWindow(id)) => {
        ctream_agent::daemon::capture_stream(target, "auto", Some(id), None, false)
      }
      (_, TargetOperation::Input | TargetOperation::InputWindow(_)) => unreachable!(),
    },
    Request::Controls(target, id, ctream_driver::ControlOperation::Act) => semantic(target, id),
    Request::Controls(target, id, inspect) => {
      ctream_platform::controls::resolve(target, id)?.run(inspect)
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
  let mut gate = Gate::delegated(target, window, &key)?;
  let mut external = match (target, window) {
    (Target::X11(display), None) => input::x11::Driver.open(&display)?.input()?,
    (Target::Wayland(display), None) => input::wayland::Driver.open(&display)?.input()?,
    (Target::X11(display), Some(id)) => {
      return native_input::serve_driver(mode, gate, move |reader, writer| {
        input::x11::Driver
          .open(&display)?
          .input_window(id)?
          .run_piped(reader, writer)
      });
    }
    _ => return Err("this agent supports Linux displays and X11 window input".into()),
  };
  gate.helper_guard()?.prepare(&mut external.0)?;
  native_input::serve_driver(mode, gate, move |reader, writer| {
    external.run_piped(reader, writer)
  })
}

fn semantic(target: Target, id: &str) -> Result<()> {
  use ctream_agent::{daemon::Authority, desktop_input::InputRequest};
  use ctream_client::sdk::{Mutation, SemanticRefusal};
  use ctream_proto::{Action, ActionResult};
  let mut payload = Vec::new();
  io::stdin().lock().take(65537).read_to_end(&mut payload)?;
  if payload.len() > 65536 {
    return Err("semantic action exceeds size limit".into());
  }
  let action: Action = serde_json::from_slice(&payload)?;
  let Ok(key) = std::env::var("CTREAM_INPUT_DELEGATION") else {
    let label = ctream_client::generate_agent_id();
    let participant_id = uuid::Uuid::new_v4().to_string();
    let mut authority = Authority::connect(target, Some(id), &label, &participant_id)?;
    let action = match action {
      Action::Invoke { path, index, name } => Mutation::Invoke { path, index, name },
      Action::SetText { path, text } => Mutation::SetText { path, text },
    };
    let result = match authority.execute(None, InputRequest::Sdk { action }) {
      Ok(_) => ActionResult {
        applied: true,
        error: None,
      },
      Err(error) => {
        let cause = error
          .get_ref()
          .and_then(|cause| cause.downcast_ref::<ctream_proto::live::ActionFailure>())
          .map(|failure| &failure.source)
          .unwrap_or(&error);
        match cause
          .get_ref()
          .and_then(|cause| cause.downcast_ref::<SemanticRefusal>())
        {
          Some(reason) => ActionResult {
            applied: false,
            error: Some(reason.0.clone()),
          },
          None => return Err(error.into()),
        }
      }
    };
    serde_json::to_writer(io::stdout().lock(), &result)?;
    return Ok(());
  };
  let native = ctream_platform::controls::resolve(target, id)?;
  let mut gate = Gate::delegated(target, Some(id), &key)?;
  let mut reply = Vec::new();
  let guard = gate.helper_guard()?;
  gate.semantic(|| {
    reply = native.apply(&payload, |command| guard.prepare(command))?;
    serde_json::from_slice::<ActionResult>(&reply).map_err(|_| {
      ctream_driver::UnconfirmedDelivery::error(io::Error::new(
        io::ErrorKind::InvalidData,
        "semantic driver returned an invalid outcome",
      ))
    })?;
    Ok(())
  })?;
  io::stdout().write_all(&reply)?;
  Ok(())
}
