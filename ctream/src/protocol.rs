use serde::{Deserialize, Serialize};
use std::fmt;

pub const VERSION: &str = "1.0.1";
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
