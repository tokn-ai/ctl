use ctream::{
  protocol::{
    DISPLAY_VERSION, Target, VERSION, WAYLAND_WINDOW_VERSION, X11_WINDOW_VERSION, valid_window_id,
  },
  providers::{
    self, AccessibleControls, CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow,
    ControlOperation, ControlSource, DiscoverWindows, ForwardInput, InputSource, Provider, Result,
    Wayland, X11,
  },
};
use std::{env, io};

enum TargetOperation<'a> {
  Capture,
  Input,
  Windows,
  CaptureWindow(&'a str),
}

enum Request<'a> {
  Discover,
  Target(Target, TargetOperation<'a>),
  Controls(Target, &'a str, ControlOperation),
}

fn supports_windows(version: &str, target: Target) -> bool {
  matches!(version, VERSION | X11_WINDOW_VERSION)
    || (version == WAYLAND_WINDOW_VERSION && matches!(target, Target::Wayland(_)))
}

fn supports_displays(version: &str) -> bool {
  matches!(
    version,
    VERSION | X11_WINDOW_VERSION | WAYLAND_WINDOW_VERSION | DISPLAY_VERSION
  )
}

fn parse(args: &[String]) -> Result<Request<'_>> {
  match args {
    [version, operation] if supports_displays(version) && operation == "discover" => {
      Ok(Request::Discover)
    }
    [version, operation, target] if supports_displays(version) => {
      let target = Target::parse(target)?;
      let operation = match operation.as_str() {
        "capture" => TargetOperation::Capture,
        "input" => TargetOperation::Input,
        "windows" if supports_windows(version, target) => TargetOperation::Windows,
        _ => return Err("unknown ctream-agent operation".into()),
      };
      Ok(Request::Target(target, operation))
    }
    [version, operation, target, id]
      if matches!(operation.as_str(), "capture-window" | "elements" | "action") =>
    {
      let target = Target::parse(target)?;
      if !supports_windows(version, target) {
        return Err("window operation unavailable in requested agent version".into());
      }
      if !valid_window_id(id) {
        return Err("invalid window identifier".into());
      }
      let request = match operation.as_str() {
        "capture-window" => Request::Target(target, TargetOperation::CaptureWindow(id)),
        "elements" => Request::Controls(
          target,
          id,
          ControlOperation::Inspect {
            bounds: version == VERSION,
          },
        ),
        _ => Request::Controls(target, id, ControlOperation::Act),
      };
      Ok(request)
    }
    _ => Err("usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]".into()),
  }
}

fn run_session<S>(session: S, operation: TargetOperation<'_>) -> Result<()>
where
  S: DiscoverWindows + CaptureDisplay + CaptureWindow + ForwardInput,
{
  match operation {
    TargetOperation::Capture => {
      let lifetime = CaptureLifetime::from_reader(io::stdin());
      session.capture_display()?.run(&mut io::stdout(), &lifetime)
    }
    TargetOperation::Input => session.input()?.run(),
    TargetOperation::Windows => {
      serde_json::to_writer(io::stdout().lock(), &session.windows()?)?;
      Ok(())
    }
    TargetOperation::CaptureWindow(id) => {
      let lifetime = CaptureLifetime::from_reader(io::stdin());
      session
        .capture_window(id)?
        .run(&mut io::stdout(), &lifetime)
    }
  }
}

fn run_controls<S: AccessibleControls>(
  session: S,
  id: &str,
  operation: ControlOperation,
) -> Result<()> {
  session.controls(id)?.run(operation)
}

pub(super) fn run() -> Result<()> {
  let args: Vec<_> = env::args().skip(1).collect();
  match parse(&args)? {
    Request::Discover => {
      serde_json::to_writer(io::stdout().lock(), &providers::discover_sessions()?)?;
      Ok(())
    }
    Request::Target(target, operation) => match target {
      Target::X11(display) => run_session(X11.open(display), operation),
      Target::Wayland(display) => run_session(Wayland.open(display), operation),
    },
    Request::Controls(target, id, operation) => match target {
      Target::Wayland(display) => run_controls(Wayland.open(display), id, operation),
      Target::X11(_) => {
        Err("semantic actions currently require a supported Wayland compositor".into())
      }
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
  }

  #[test]
  fn retained_contracts_accept_only_their_window_capabilities() {
    for version in [
      DISPLAY_VERSION,
      WAYLAND_WINDOW_VERSION,
      X11_WINDOW_VERSION,
      VERSION,
    ] {
      assert!(parse(&args(&[version, "discover"])).is_ok());
      for target in [":7", "wayland-1"] {
        for operation in ["capture", "input"] {
          assert!(parse(&args(&[version, operation, target])).is_ok());
        }
        let windows = version != DISPLAY_VERSION
          && (version != WAYLAND_WINDOW_VERSION || target == "wayland-1");
        assert_eq!(parse(&args(&[version, "windows", target])).is_ok(), windows);
        assert_eq!(
          parse(&args(&[version, "capture-window", target, "window-1"])).is_ok(),
          windows
        );
      }
    }
  }

  #[test]
  fn bounds_are_requested_only_under_the_bounds_contract() {
    for version in [WAYLAND_WINDOW_VERSION, X11_WINDOW_VERSION, VERSION] {
      let arguments = args(&[version, "elements", "wayland-1", "window-1"]);
      let Request::Controls(_, _, ControlOperation::Inspect { bounds }) =
        parse(&arguments).unwrap()
      else {
        panic!("expected inspection");
      };
      assert_eq!(bounds, version == VERSION);
    }
    assert!(parse(&args(&[VERSION, "capture-window", ":7", "invalid/id"])).is_err());
    assert!(parse(&args(&["9.0.9", "capture", ":7"])).is_err());
  }
}
