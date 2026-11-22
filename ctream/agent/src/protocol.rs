use ctream_driver::{ControlOperation, Result};
use ctream_proto::{
  BOUNDS_VERSION, CAPTURE_CATALOG_VERSION, CAPTURE_DRIVER_VERSION, DISPLAY_VERSION,
  LATENCY_VERSION, MACOS_WINDOW_VERSION, Target, WAYLAND_WINDOW_VERSION, WINDOWS_DESKTOP_VERSION,
  WINDOWS_DISPLAY_VERSION, WINDOWS_WINDOW_VERSION, WindowId, X11_WINDOW_VERSION,
  valid_capture_driver, valid_window_id,
};

pub enum TargetOperation<'a> {
  Capture,
  Input,
  Windows,
  CaptureWindow(&'a str),
  InputWindow(&'a str),
}

pub enum Request<'a> {
  Ping,
  Discover {
    windows_displays: bool,
    macos: bool,
  },
  CaptureCatalog {
    macos: bool,
  },
  CaptureDrivers(Target),
  CaptureWithDriver {
    target: Target,
    driver: &'a str,
    window: Option<&'a str>,
  },
  Target(Target, TargetOperation<'a>),
  Controls(Target, &'a str, ControlOperation),
}

fn supports_windows(version: &str, target: Target) -> bool {
  match target {
    Target::WindowsDesktop => version == WINDOWS_DESKTOP_VERSION,
    Target::Windows(_) => matches!(
      version,
      WINDOWS_WINDOW_VERSION
        | CAPTURE_DRIVER_VERSION
        | CAPTURE_CATALOG_VERSION
        | WINDOWS_DESKTOP_VERSION
    ),
    _ => {
      matches!(
        version,
        WINDOWS_DESKTOP_VERSION
          | CAPTURE_CATALOG_VERSION
          | CAPTURE_DRIVER_VERSION
          | WINDOWS_WINDOW_VERSION
          | WINDOWS_DISPLAY_VERSION
          | BOUNDS_VERSION
          | X11_WINDOW_VERSION
      ) || (version == WAYLAND_WINDOW_VERSION && matches!(target, Target::Wayland(_)))
    }
  }
}

fn supports_displays(version: &str) -> bool {
  matches!(
    version,
    WINDOWS_DESKTOP_VERSION
      | CAPTURE_CATALOG_VERSION
      | CAPTURE_DRIVER_VERSION
      | WINDOWS_WINDOW_VERSION
      | WINDOWS_DISPLAY_VERSION
      | BOUNDS_VERSION
      | X11_WINDOW_VERSION
      | WAYLAND_WINDOW_VERSION
      | DISPLAY_VERSION
  )
}

fn supports_windows_displays(version: &str) -> bool {
  matches!(
    version,
    WINDOWS_DISPLAY_VERSION
      | WINDOWS_WINDOW_VERSION
      | CAPTURE_DRIVER_VERSION
      | CAPTURE_CATALOG_VERSION
      | WINDOWS_DESKTOP_VERSION
  )
}

fn parse_target(version: &str, value: &str) -> Result<Target> {
  match Target::parse(value)? {
    Target::Windows(_) if !supports_windows_displays(version) => {
      Err("Windows displays require ctream_agent 1.0.5".into())
    }
    Target::WindowsDesktop if version != WINDOWS_DESKTOP_VERSION => {
      Err("Windows desktop-wide window selection requires ctream_agent 1.0.9".into())
    }
    Target::MacosDesktop => Err("macOS window selection requires ctream_agent 1.0.11".into()),
    target => Ok(target),
  }
}

fn supports_capture_selection(version: &str, target: &str) -> bool {
  version == CAPTURE_DRIVER_VERSION || (version == WINDOWS_DESKTOP_VERSION && target == "windows")
}

pub fn parse(args: &[String]) -> Result<Request<'_>> {
  match args {
    [version, operation, tail @ ..] if version == MACOS_WINDOW_VERSION => {
      parse_macos_version(operation, tail)
    }
    [version, operation] if version == LATENCY_VERSION && operation == "ping" => Ok(Request::Ping),
    [version, operation]
      if version == CAPTURE_CATALOG_VERSION && operation == "capture-drivers" =>
    {
      Ok(Request::CaptureCatalog { macos: false })
    }
    [version, operation, target]
      if operation == "capture-drivers" && supports_capture_selection(version, target) =>
    {
      Ok(Request::CaptureDrivers(parse_target(version, target)?))
    }
    [version, operation, target, driver, tail @ ..]
      if operation == "capture-with-driver"
        && supports_capture_selection(version, target)
        && tail.len() <= 1 =>
    {
      if !valid_capture_driver(driver) {
        return Err("invalid capture driver identifier".into());
      }
      let target = parse_target(version, target)?;
      let window = tail.first().map(String::as_str);
      if target == Target::WindowsDesktop && window.is_none() {
        return Err("Windows desktop target requires a window ID".into());
      }
      if window.is_some_and(|id| !valid_window_id(id)) {
        return Err("invalid window identifier".into());
      }
      Ok(Request::CaptureWithDriver {
        target,
        driver,
        window,
      })
    }
    [version, operation] if supports_displays(version) && operation == "discover" => {
      Ok(Request::Discover {
        windows_displays: supports_windows_displays(version),
        macos: false,
      })
    }
    [version, operation, target] if supports_displays(version) => {
      let target = parse_target(version, target)?;
      let operation = match operation.as_str() {
        "capture" if target != Target::WindowsDesktop => TargetOperation::Capture,
        "input" if target != Target::WindowsDesktop => TargetOperation::Input,
        "windows" if supports_windows(version, target) => TargetOperation::Windows,
        _ => return Err("unknown ctream-agent operation".into()),
      };
      Ok(Request::Target(target, operation))
    }
    [version, operation, target, id]
      if matches!(
        operation.as_str(),
        "capture-window" | "input-window" | "elements" | "action"
      ) =>
    {
      let target = parse_target(version, target)?;
      if !supports_windows(version, target) {
        return Err("window operation unavailable in requested agent version".into());
      }
      if !valid_window_id(id) {
        return Err("invalid window identifier".into());
      }
      let request = match operation.as_str() {
        "capture-window" => Request::Target(target, TargetOperation::CaptureWindow(id)),
        "input-window" if matches!(target, Target::Windows(_) | Target::WindowsDesktop) => {
          Request::Target(target, TargetOperation::InputWindow(id))
        }
        "input-window" => return Err("window input requires a Windows window".into()),
        _ if matches!(target, Target::Windows(_) | Target::WindowsDesktop) => {
          return Err("Windows accessibility operations are not supported".into());
        }
        "elements" => Request::Controls(
          target,
          id,
          ControlOperation::Inspect {
            bounds: matches!(
              version.as_str(),
              BOUNDS_VERSION
                | WINDOWS_DISPLAY_VERSION
                | WINDOWS_WINDOW_VERSION
                | CAPTURE_DRIVER_VERSION
                | CAPTURE_CATALOG_VERSION
                | WINDOWS_DESKTOP_VERSION
            ),
          },
        ),
        _ => Request::Controls(target, id, ControlOperation::Act),
      };
      Ok(request)
    }
    _ => Err("usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]".into()),
  }
}

fn parse_macos_version<'a>(operation: &str, tail: &'a [String]) -> Result<Request<'a>> {
  let target = Target::MacosDesktop;
  match (operation, tail) {
    ("discover", []) => Ok(Request::Discover {
      windows_displays: true,
      macos: true,
    }),
    ("capture-drivers", []) => Ok(Request::CaptureCatalog { macos: true }),
    ("capture-drivers", [name]) if name == "macos" => Ok(Request::CaptureDrivers(target)),
    ("windows", [name]) if name == "macos" => Ok(Request::Target(target, TargetOperation::Windows)),
    ("capture-window" | "input-window", [name, id]) if name == "macos" => {
      validate_macos_window(id)?;
      let operation = match operation {
        "capture-window" => TargetOperation::CaptureWindow(id),
        _ => TargetOperation::InputWindow(id),
      };
      Ok(Request::Target(target, operation))
    }
    ("capture-with-driver", [name, driver, id]) if name == "macos" => {
      if !valid_capture_driver(driver) {
        return Err("invalid capture driver identifier".into());
      }
      validate_macos_window(id)?;
      Ok(Request::CaptureWithDriver {
        target,
        driver,
        window: Some(id),
      })
    }
    _ => Err("unknown ctream-agent 1.0.11 operation".into()),
  }
}

fn validate_macos_window(id: &str) -> Result<()> {
  WindowId::macos_parts(id).ok_or("invalid macOS window identifier")?;
  Ok(())
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
      BOUNDS_VERSION,
      WINDOWS_DISPLAY_VERSION,
      WINDOWS_WINDOW_VERSION,
      CAPTURE_DRIVER_VERSION,
      CAPTURE_CATALOG_VERSION,
      WINDOWS_DESKTOP_VERSION,
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
        assert!(parse(&args(&[version, "input-window", target, "window-1"])).is_err());
      }
    }
  }

  #[test]
  fn bounds_are_requested_only_under_the_bounds_contract() {
    for version in [
      WAYLAND_WINDOW_VERSION,
      X11_WINDOW_VERSION,
      BOUNDS_VERSION,
      WINDOWS_DISPLAY_VERSION,
      WINDOWS_WINDOW_VERSION,
      CAPTURE_DRIVER_VERSION,
      CAPTURE_CATALOG_VERSION,
      WINDOWS_DESKTOP_VERSION,
    ] {
      let arguments = args(&[version, "elements", "wayland-1", "window-1"]);
      let Request::Controls(_, _, ControlOperation::Inspect { bounds }) =
        parse(&arguments).unwrap()
      else {
        panic!("expected inspection");
      };
      assert_eq!(
        bounds,
        matches!(
          version,
          BOUNDS_VERSION
            | WINDOWS_DISPLAY_VERSION
            | WINDOWS_WINDOW_VERSION
            | CAPTURE_DRIVER_VERSION
            | CAPTURE_CATALOG_VERSION
            | WINDOWS_DESKTOP_VERSION
        )
      );
    }
    assert!(
      parse(&args(&[
        WINDOWS_DESKTOP_VERSION,
        "capture-window",
        ":7",
        "invalid/id"
      ]))
      .is_err()
    );
    assert!(parse(&args(&["9.0.9", "capture", ":7"])).is_err());
  }

  #[test]
  fn windows_display_and_window_operations_require_their_own_contracts() {
    for version in [
      DISPLAY_VERSION,
      WAYLAND_WINDOW_VERSION,
      X11_WINDOW_VERSION,
      BOUNDS_VERSION,
      WINDOWS_DISPLAY_VERSION,
      WINDOWS_WINDOW_VERSION,
      CAPTURE_DRIVER_VERSION,
      CAPTURE_CATALOG_VERSION,
      WINDOWS_DESKTOP_VERSION,
    ] {
      let arguments = args(&[version, "discover"]);
      let Request::Discover {
        windows_displays,
        macos: false,
      } = parse(&arguments).unwrap()
      else {
        panic!("expected discovery");
      };
      assert_eq!(
        windows_displays,
        matches!(
          version,
          WINDOWS_DISPLAY_VERSION
            | WINDOWS_WINDOW_VERSION
            | CAPTURE_DRIVER_VERSION
            | CAPTURE_CATALOG_VERSION
            | WINDOWS_DESKTOP_VERSION
        )
      );
      for operation in ["capture", "input"] {
        assert_eq!(
          parse(&args(&[version, operation, "windows-1"])).is_ok(),
          windows_displays
        );
      }
      let window_operations = matches!(
        version,
        WINDOWS_WINDOW_VERSION
          | CAPTURE_DRIVER_VERSION
          | CAPTURE_CATALOG_VERSION
          | WINDOWS_DESKTOP_VERSION
      );
      assert_eq!(
        parse(&args(&[version, "windows", "windows-1"])).is_ok(),
        window_operations
      );
      for operation in ["capture-window", "input-window"] {
        assert_eq!(
          parse(&args(&[version, operation, "windows-1", "window-1"])).is_ok(),
          window_operations
        );
      }
      for operation in ["elements", "action"] {
        assert!(parse(&args(&[version, operation, "windows-1", "window-1"])).is_err());
      }
    }
    assert!(
      parse(&args(&[
        WINDOWS_DESKTOP_VERSION,
        "input-window",
        "windows-1",
        "invalid/id"
      ]))
      .is_err()
    );
  }

  #[test]
  fn ping_requires_its_own_version_and_no_target() {
    assert!(matches!(
      parse(&args(&[LATENCY_VERSION, "ping"])).unwrap(),
      Request::Ping
    ));
    assert!(parse(&args(&[WINDOWS_DESKTOP_VERSION, "ping"])).is_err());
    assert!(parse(&args(&[LATENCY_VERSION, "ping", "windows"])).is_err());
  }

  #[test]
  fn windows_desktop_accepts_only_window_operations_under_its_contract() {
    let desktop = Target::WindowsDesktop;
    let version = WINDOWS_DESKTOP_VERSION;
    assert!(matches!(
      parse(&args(&[version, "windows", "windows"])).unwrap(),
      Request::Target(Target::WindowsDesktop, TargetOperation::Windows)
    ));
    assert!(matches!(
      parse(&args(&[version, "capture-drivers", "windows"])).unwrap(),
      Request::CaptureDrivers(Target::WindowsDesktop)
    ));
    let capture = [
      version,
      "capture-with-driver",
      "windows",
      "auto",
      "window-1",
    ];
    assert!(matches!(
      parse(&args(&capture)).unwrap(),
      Request::CaptureWithDriver { target, window: Some("window-1"), .. } if target == desktop
    ));
    for operation in ["capture-window", "input-window"] {
      assert!(parse(&args(&[version, operation, "windows", "window-1"])).is_ok());
    }
    for unsupported in [
      vec![version, "capture", "windows"],
      vec![version, "input", "windows"],
      vec![version, "capture-with-driver", "windows", "auto"],
      vec![version, "input-window", "windows", "invalid/id"],
      vec![version, "elements", "windows", "window-1"],
      vec![
        WINDOWS_WINDOW_VERSION,
        "capture-window",
        "windows",
        "window-1",
      ],
      vec![CAPTURE_DRIVER_VERSION, "capture-drivers", "windows"],
      vec![CAPTURE_CATALOG_VERSION, "windows", "windows"],
    ] {
      assert!(parse(&args(&unsupported)).is_err(), "{unsupported:?}");
    }
  }

  #[test]
  fn target_independent_catalog_requires_its_own_contract_and_exact_shape() {
    assert!(matches!(
      parse(&args(&[CAPTURE_CATALOG_VERSION, "capture-drivers"])).unwrap(),
      Request::CaptureCatalog { macos: false }
    ));
    for old in [
      DISPLAY_VERSION,
      WAYLAND_WINDOW_VERSION,
      X11_WINDOW_VERSION,
      BOUNDS_VERSION,
      WINDOWS_DISPLAY_VERSION,
      WINDOWS_WINDOW_VERSION,
      CAPTURE_DRIVER_VERSION,
      WINDOWS_DESKTOP_VERSION,
    ] {
      assert!(parse(&args(&[old, "capture-drivers"])).is_err());
    }
    for target in [":7", "wayland-1", "windows-2"] {
      assert!(parse(&args(&[CAPTURE_CATALOG_VERSION, "capture-drivers", target])).is_err());
      assert!(matches!(
        parse(&args(&[CAPTURE_DRIVER_VERSION, "capture-drivers", target])).unwrap(),
        Request::CaptureDrivers(_)
      ));
    }
  }

  #[test]
  fn macos_discovery_and_window_operations_require_the_new_contract() {
    let version = MACOS_WINDOW_VERSION;
    let name = "macos";
    let window = "macos-123-42";
    assert!(matches!(
      parse(&args(&[version, "discover"])).unwrap(),
      Request::Discover { macos: true, .. }
    ));
    assert!(matches!(
      parse(&args(&[version, "capture-drivers"])).unwrap(),
      Request::CaptureCatalog { macos: true }
    ));
    assert!(matches!(
      parse(&args(&[version, "capture-drivers", name])).unwrap(),
      Request::CaptureDrivers(Target::MacosDesktop)
    ));
    assert!(matches!(
      parse(&args(&[version, "windows", name])).unwrap(),
      Request::Target(Target::MacosDesktop, TargetOperation::Windows)
    ));
    assert!(matches!(
      parse(&args(&[
        version,
        "capture-with-driver",
        name,
        "auto",
        window
      ]))
      .unwrap(),
      Request::CaptureWithDriver {
        target: Target::MacosDesktop,
        window: Some("macos-123-42"),
        ..
      }
    ));
    assert!(matches!(
      parse(&args(&[version, "capture-window", name, window])).unwrap(),
      Request::Target(
        Target::MacosDesktop,
        TargetOperation::CaptureWindow("macos-123-42")
      )
    ));
    assert!(matches!(
      parse(&args(&[version, "input-window", name, window])).unwrap(),
      Request::Target(
        Target::MacosDesktop,
        TargetOperation::InputWindow("macos-123-42")
      )
    ));
    for unsupported in [
      vec![LATENCY_VERSION, "discover"],
      vec![WINDOWS_DESKTOP_VERSION, "windows", name],
      vec![
        CAPTURE_DRIVER_VERSION,
        "capture-with-driver",
        name,
        "auto",
        window,
      ],
      vec![WINDOWS_DESKTOP_VERSION, "input-window", name, window],
      vec![version, "input", name],
      vec![version, "capture", name],
      vec![version, "capture-with-driver", name, "auto"],
      vec![version, "capture-with-driver", name, "bad/driver", window],
      vec![version, "input-window", name, "window_1"],
      vec![version, "capture-window", name, "macos-0-42"],
      vec![version, "elements", name, window],
      vec![version, "action", name, window],
    ] {
      assert!(parse(&args(&unsupported)).is_err(), "{unsupported:?}");
    }
  }

  #[test]
  fn capture_selection_requires_its_contract_and_bounded_identifiers() {
    for target in [":7", "wayland-1", "windows-2"] {
      assert!(matches!(
        parse(&args(&[CAPTURE_DRIVER_VERSION, "capture-drivers", target])).unwrap(),
        Request::CaptureDrivers(_)
      ));
      assert!(matches!(
        parse(&args(&[
          CAPTURE_DRIVER_VERSION,
          "capture-with-driver",
          target,
          "user.capture"
        ]))
        .unwrap(),
        Request::CaptureWithDriver { window: None, .. }
      ));
      assert!(matches!(
        parse(&args(&[
          CAPTURE_DRIVER_VERSION,
          "capture-with-driver",
          target,
          "auto",
          "window-1"
        ]))
        .unwrap(),
        Request::CaptureWithDriver {
          window: Some("window-1"),
          ..
        }
      ));
      for old in [
        DISPLAY_VERSION,
        WAYLAND_WINDOW_VERSION,
        X11_WINDOW_VERSION,
        BOUNDS_VERSION,
        WINDOWS_DISPLAY_VERSION,
        WINDOWS_WINDOW_VERSION,
      ] {
        assert!(parse(&args(&[old, "capture-drivers", target])).is_err());
        assert!(parse(&args(&[old, "capture-with-driver", target, "auto"])).is_err());
      }
    }
    for arguments in [
      vec![CAPTURE_DRIVER_VERSION, "capture-with-driver", ":7", ""],
      vec![
        CAPTURE_DRIVER_VERSION,
        "capture-with-driver",
        ":7",
        "driver;command",
      ],
      vec![
        CAPTURE_DRIVER_VERSION,
        "capture-with-driver",
        ":7",
        "auto",
        "bad/id",
      ],
      vec![
        CAPTURE_DRIVER_VERSION,
        "capture-with-driver",
        ":7",
        "auto",
        "window-1",
        "extra",
      ],
    ] {
      assert!(parse(&args(&arguments)).is_err());
    }
  }
}
