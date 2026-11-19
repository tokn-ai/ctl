use serde::{Deserialize, Serialize};
use std::fmt;

pub const VERSION: &str = "1.0.3";
pub const WAYLAND_WINDOW_VERSION: &str = "1.0.2";
pub const X11_FRAME_SIZE: [usize; 2] = [960, 540];

#[derive(Clone, Copy)]
pub enum Target {
  X11(u32),
  Wayland(u32),
}

impl Target {
  pub fn parse(name: &str) -> Result<Self, &'static str> {
    let (digits, wayland) = if let Some(digits) = name.strip_prefix(':') {
      (digits, false)
    } else if let Some(digits) = name.strip_prefix("wayland-") {
      (digits, true)
    } else {
      return Err("expected :DISPLAY or wayland-N");
    };
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
      return Err("invalid display number");
    }
    let number = digits.parse().map_err(|_| "invalid display number")?;
    Ok(if wayland {
      Self::Wayland(number)
    } else {
      Self::X11(number)
    })
  }
}

impl fmt::Display for Target {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::X11(number) => write!(f, ":{number}"),
      Self::Wayland(number) => write!(f, "wayland-{number}"),
    }
  }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Session {
  pub display: super::Display,
  pub class: Option<String>,
  pub selectable: bool,
}

pub const DISPLAY_VERSION: &str = "1.0.1";

#[derive(Clone, Debug, Deserialize, Serialize)]
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
