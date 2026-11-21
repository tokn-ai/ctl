//! Shared data and wire contracts for ctream clients and agents.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const VERSION: &str = LATENCY_VERSION;
pub const LATENCY_VERSION: &str = "1.0.10";
pub const WINDOWS_DESKTOP_VERSION: &str = "1.0.9";
pub const CAPTURE_CATALOG_VERSION: &str = "1.0.8";
pub const CAPTURE_DRIVER_VERSION: &str = "1.0.7";
pub const WINDOWS_WINDOW_VERSION: &str = "1.0.6";
pub const WINDOWS_DISPLAY_VERSION: &str = "1.0.5";
pub const BOUNDS_VERSION: &str = "1.0.4";
pub const X11_WINDOW_VERSION: &str = "1.0.3";
pub const WAYLAND_WINDOW_VERSION: &str = "1.0.2";
pub const X11_FRAME_SIZE: [usize; 2] = [960, 540];

pub const PING_HELLO: u8 = b'!';
pub const PING_TOKEN: u8 = b'?';

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureScope {
  Display,
  Window,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureDriverInfo {
  pub id: String,
  pub name: String,
  pub scopes: Vec<CaptureScope>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSystem {
  X11,
  Wayland,
  Windows,
}

/// A compiled provider's capabilities, independent of any running session.
/// Custom providers declare their systems explicitly; IDs carry no semantics.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureDriverDescriptor {
  #[serde(flatten)]
  pub info: CaptureDriverInfo,
  pub systems: Vec<CaptureSystem>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CaptureFormat {
  Ppm,
  FixedRgb { size: [usize; 2] },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureStarted {
  pub driver: String,
  pub format: CaptureFormat,
}

pub const CAPTURE_STARTED_LIMIT: usize = 4096;

/// Provider IDs remain extensible strings, independent of the bundled catalog.
pub fn valid_capture_driver(id: &str) -> bool {
  !id.is_empty()
    && id.len() <= 128
    && id
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Display {
  X11(u32),
  Wayland {
    uid: u32,
    socket: String,
  },
  Windows {
    number: u32,
    name: String,
    /// Physical desktop coordinates: left, top, right, bottom.
    bounds: [i32; 4],
  },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
  X11(u32),
  Wayland(u32),
  Windows(u32),
  /// Desktop-wide window selection. Display capture still uses a monitor.
  WindowsDesktop,
}

impl Target {
  pub const fn system(self) -> CaptureSystem {
    match self {
      Self::X11(_) => CaptureSystem::X11,
      Self::Wayland(_) => CaptureSystem::Wayland,
      Self::Windows(_) | Self::WindowsDesktop => CaptureSystem::Windows,
    }
  }

  pub fn parse(name: &str) -> Result<Self, &'static str> {
    if name == "windows" {
      return Ok(Self::WindowsDesktop);
    }
    let (digits, constructor): (_, fn(u32) -> Self) = if let Some(digits) = name.strip_prefix(':') {
      (digits, Self::X11)
    } else if let Some(digits) = name.strip_prefix("wayland-") {
      (digits, Self::Wayland)
    } else if let Some(digits) = name.strip_prefix("windows-") {
      (digits, Self::Windows)
    } else {
      return Err("expected :DISPLAY, wayland-N, windows-N, or windows");
    };
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
      return Err("invalid display number");
    }
    let number = digits.parse().map_err(|_| "invalid display number")?;
    let target = constructor(number);
    if target == Self::Windows(0) {
      return Err("Windows display numbers start at 1");
    }
    Ok(target)
  }
}

impl fmt::Display for Target {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::X11(number) => write!(f, ":{number}"),
      Self::Wayland(number) => write!(f, "wayland-{number}"),
      Self::Windows(number) => write!(f, "windows-{number}"),
      Self::WindowsDesktop => f.write_str("windows"),
    }
  }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Session {
  pub display: Display,
  pub class: Option<String>,
  pub selectable: bool,
}

pub const DISPLAY_VERSION: &str = "1.0.1";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Window {
  pub id: String,
  pub title: String,
  pub app_id: String,
}

pub fn valid_window_id(id: &str) -> bool {
  !id.is_empty()
    && id.len() <= 128
    && id
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Element {
  pub path: String,
  pub name: String,
  pub role: String,
  pub actions: Vec<String>,
  pub editable: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub bounds: Option<ControlBounds>,
}

/// Window capture coordinates expressed as fractions of the image dimensions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct ControlBounds {
  pub x: f32,
  pub y: f32,
  pub width: f32,
  pub height: f32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Action {
  Invoke {
    path: String,
    index: usize,
    name: String,
  },
  SetText {
    path: String,
    text: String,
  },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ActionResult {
  pub applied: bool,
  pub error: Option<String>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn target_names_round_trip_and_reject_shell_text() {
    for (target, system) in [
      (Target::X11(0), CaptureSystem::X11),
      (Target::Wayland(1), CaptureSystem::Wayland),
      (Target::Windows(2), CaptureSystem::Windows),
      (Target::WindowsDesktop, CaptureSystem::Windows),
    ] {
      assert_eq!(Target::parse(&target.to_string()).unwrap(), target);
      assert_eq!(target.system(), system);
    }
    for invalid in [
      "windows-0",
      "windows-",
      "windows--1",
      "windows-1;id",
      "windows-4294967296",
    ] {
      assert!(Target::parse(invalid).is_err(), "{invalid}");
    }
  }

  #[test]
  fn catalog_metadata_is_explicit_for_custom_multisystem_drivers() {
    let wire = r#"{"id":"user.surface","name":"User surface","scopes":["display","window"],"systems":["x11","windows"]}"#;
    let driver: CaptureDriverDescriptor = serde_json::from_str(wire).unwrap();
    assert_eq!(driver.info.id, "user.surface");
    assert_eq!(driver.systems, [CaptureSystem::X11, CaptureSystem::Windows]);
    assert_eq!(
      driver.info.scopes,
      [CaptureScope::Display, CaptureScope::Window]
    );
    assert_eq!(serde_json::to_string(&driver).unwrap(), wire);

    // The retained target catalog keeps its original fields and has no systems.
    assert_eq!(
      serde_json::to_string(&driver.info).unwrap(),
      r#"{"id":"user.surface","name":"User surface","scopes":["display","window"]}"#
    );
  }

  #[test]
  fn windows_discovery_preserves_physical_bounds_and_linux_shapes() {
    let display = Display::Windows {
      number: 2,
      name: "display".into(),
      bounds: [-1920, 0, 0, 1080],
    };
    let bytes = serde_json::to_vec(&display).unwrap();
    assert_eq!(serde_json::from_slice::<Display>(&bytes).unwrap(), display);
    assert_eq!(
      serde_json::to_string(&Display::X11(7)).unwrap(),
      r#"{"x11":7}"#
    );
  }
}
