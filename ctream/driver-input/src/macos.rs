//! Process-targeted Quartz events for ScreenCaptureKit window control.
//!
//! Delivery depends on the target application's input policy. An inactive
//! AppKit view can decline a synthetic mouse press in `acceptsFirstMouse:`.

mod keys;

use std::{
  collections::BTreeSet,
  io,
  time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowIdentity {
  pub id: u32,
  pub pid: i32,
}

#[repr(C)]
struct Target {
  id: u32,
  pid: i32,
  started_seconds: u64,
  started_microseconds: u64,
}

#[repr(u8)]
#[derive(Clone, Copy)]
enum PointerEvent {
  Move,
  Down,
  Drag,
  Up,
  Scroll,
}

unsafe extern "C" {
  fn ctream_macos_input_has_access() -> bool;
  fn ctream_macos_input_bind(target: &mut Target) -> bool;
  fn ctream_macos_input_key(
    target: &Target,
    code: u16,
    pressed: bool,
    modifier: bool,
    flags: u64,
  ) -> i32;
  fn ctream_macos_input_pointer(
    target: &Target,
    event: PointerEvent,
    button: u8,
    fraction: &[f64; 2],
    position: &mut [f64; 2],
    flags: u64,
    click_count: u32,
  ) -> i32;
}

#[derive(Clone, Copy)]
struct FramePoint {
  fraction: [f64; 2],
  size: [u32; 2],
}

impl FramePoint {
  fn new(point: [u32; 2], size: [u32; 2]) -> io::Result<Self> {
    if point[0] >= size[0] || point[1] >= size[1] {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "pointer is outside the capture dimensions",
      ));
    }
    // Captures use physical pixels. Quartz positions use desktop points.
    // A pixel's center maps into the window's current point bounds.
    Ok(Self {
      fraction: std::array::from_fn(|axis| (f64::from(point[axis]) + 0.5) / f64::from(size[axis])),
      size,
    })
  }

  fn near(self, other: Self) -> bool {
    (0..2).all(|axis| {
      let extent = f64::from(self.size[axis].max(other.size[axis]));
      (self.fraction[axis] - other.fraction[axis]).abs() * extent <= 4.0
    })
  }
}

#[derive(Clone, Copy, Default)]
struct ButtonPress {
  position: [f64; 2],
  click_count: u32,
}

struct Click {
  button: u8,
  point: FramePoint,
  at: Instant,
  count: u32,
}

pub struct WindowInput {
  target: Target,
  require_delivery: bool,
  pointer: Option<FramePoint>,
  buttons: [Option<ButtonPress>; 5],
  keys: BTreeSet<u16>,
  last_click: Option<Click>,
}

/// Preflight the current process's posting and Accessibility authorization.
/// This never prompts for or grants permission.
pub fn has_access() -> bool {
  // The native function has no arguments and returns a C bool.
  unsafe { ctream_macos_input_has_access() }
}

impl WindowInput {
  pub fn open(identity: WindowIdentity) -> io::Result<Self> {
    Self::open_with_delivery(identity, false)
  }

  /// Acknowledged input reports errors for unknown keys or rejected window routing.
  pub fn open_with_delivery(identity: WindowIdentity, require_delivery: bool) -> io::Result<Self> {
    if !has_access() {
      return Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "control requires macOS Accessibility permission for the ctream host process",
      ));
    }
    let mut target = Target {
      id: identity.id,
      pid: identity.pid,
      started_seconds: 0,
      started_microseconds: 0,
    };
    // Target matches the native structure, which the function fills in place.
    if !unsafe { ctream_macos_input_bind(&mut target) } {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot bind control to the selected macOS window",
      ));
    }
    Ok(Self {
      target,
      require_delivery,
      pointer: None,
      buttons: [None; 5],
      keys: BTreeSet::new(),
      last_click: None,
    })
  }

  /// Apply one retained wire event to this already process-bound window.
  pub fn dispatch(&mut self, line: &str) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid macOS input command");
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    match fields.as_slice() {
      ["move", x, y, width, height] => {
        let value = |s: &str| s.parse::<u32>().map_err(|_| invalid());
        self.move_pointer(value(x)?, value(y)?, [value(width)?, value(height)?])
      }
      ["button", code, pressed @ ("0" | "1")] => {
        self.button(code.parse().map_err(|_| invalid())?, *pressed == "1")
      }
      ["key", name, pressed @ ("0" | "1")] => self.key(name, *pressed == "1"),
      ["reset"] => self.reset(),
      _ => Err(invalid()),
    }
  }

  pub fn move_pointer(&mut self, x: u32, y: u32, size: [u32; 2]) -> io::Result<()> {
    let point = FramePoint::new([x, y], size)?;
    self.pointer = Some(point);
    if self.buttons.iter().all(Option::is_none) {
      self.post_pointer(
        PointerEvent::Move,
        0,
        point.fraction,
        &mut ButtonPress::default(),
      )?;
      return Ok(());
    }
    for (button, press) in self.buttons.into_iter().enumerate() {
      if let Some(mut press) = press
        && self.post_pointer(PointerEvent::Drag, button as u8, point.fraction, &mut press)?
      {
        self.buttons[button] = Some(press);
      }
    }
    Ok(())
  }

  pub fn button(&mut self, code: u8, pressed: bool) -> io::Result<()> {
    if self.require_delivery && pressed && self.pointer.is_none() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "set a pointer position before clicking or scrolling",
      ));
    }
    let button = match code {
      1 => 0,
      2 => 2,
      3 => 1,
      8 => 3,
      9 => 4,
      4..=7 => {
        if let (true, Some(point)) = (pressed, self.pointer) {
          self.post_pointer(
            PointerEvent::Scroll,
            code,
            point.fraction,
            &mut ButtonPress::default(),
          )?;
        }
        return Ok(());
      }
      _ => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "unknown mouse button",
        ));
      }
    };
    if !pressed {
      return self.release_button(button);
    }
    let Some(point) = self.pointer else {
      return Ok(());
    };
    if self.buttons[usize::from(button)].is_some() {
      return Ok(());
    }
    let now = Instant::now();
    let count = self.last_click.as_ref().map_or(1, |last| {
      if last.button == button
        && now.duration_since(last.at) <= Duration::from_millis(500)
        && point.near(last.point)
      {
        last.count.saturating_add(1)
      } else {
        1
      }
    });
    let mut press = ButtonPress {
      click_count: count,
      ..ButtonPress::default()
    };
    if self.post_pointer(PointerEvent::Down, button, point.fraction, &mut press)? {
      self.buttons[usize::from(button)] = Some(press);
      self.last_click = Some(Click {
        button,
        point,
        at: now,
        count,
      });
    }
    Ok(())
  }

  pub fn key(&mut self, name: &str, pressed: bool) -> io::Result<()> {
    if let Some(code) = keys::code(name) {
      self.set_key(code, pressed)?;
    } else if self.require_delivery {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "unsupported macOS key",
      ));
    }
    Ok(())
  }

  pub fn reset(&mut self) -> io::Result<()> {
    self.pointer = None;
    self.last_click = None;
    let mut result = Ok(());
    for button in 0..self.buttons.len() as u8 {
      result = result.and(self.release_button(button));
    }
    let mut held: Vec<_> = self.keys.iter().copied().collect();
    held.sort_by_key(|&code| keys::modifier(code) != 0);
    for code in held {
      result = result.and(self.set_key(code, false));
    }
    result
  }

  fn release_button(&mut self, button: u8) -> io::Result<()> {
    if let Some(mut press) = self.buttons[usize::from(button)] {
      self.post_pointer(PointerEvent::Up, button, [0.0; 2], &mut press)?;
      self.buttons[usize::from(button)] = None;
    }
    Ok(())
  }

  fn post_pointer(
    &self,
    event: PointerEvent,
    button: u8,
    fraction: [f64; 2],
    press: &mut ButtonPress,
  ) -> io::Result<bool> {
    // The native call only borrows the initialized data for this call. It
    // validates the target again and records the last delivered point.
    posted(
      unsafe {
        ctream_macos_input_pointer(
          &self.target,
          event,
          button,
          &fraction,
          &mut press.position,
          self.modifier_flags(),
          press.click_count,
        )
      },
      self.require_delivery,
    )
  }

  fn set_key(&mut self, code: u16, pressed: bool) -> io::Result<()> {
    if self.keys.contains(&code) == pressed {
      return Ok(());
    }
    let modifier = keys::modifier(code);
    let held = self.keys.iter().copied().filter(|&held| held != code);
    let flags =
      held.fold(0, |flags, key| flags | keys::modifier(key)) | if pressed { modifier } else { 0 };
    // Target matches the native structure and stays borrowed for this call.
    let sent = posted(
      unsafe { ctream_macos_input_key(&self.target, code, pressed, modifier != 0, flags) },
      self.require_delivery,
    )?;
    if !pressed {
      self.keys.remove(&code);
    } else if sent {
      self.keys.insert(code);
    }
    Ok(())
  }

  fn modifier_flags(&self) -> u64 {
    self
      .keys
      .iter()
      .fold(0, |flags, &key| flags | keys::modifier(key))
  }
}

impl Drop for WindowInput {
  fn drop(&mut self) {
    let _ = self.reset();
  }
}

fn posted(status: i32, require_delivery: bool) -> io::Result<bool> {
  match status {
    1 => Ok(true),
    0 if require_delivery => Err(io::Error::other(
      "macOS input could not be posted to the selected window",
    )),
    0 => Ok(false),
    _ => Err(io::Error::other("cannot create a macOS input event")),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pixel_centers_stay_inside_a_scaled_window() {
    let first = FramePoint::new([0, 0], [400, 600]).unwrap();
    let last = FramePoint::new([399, 599], [400, 600]).unwrap();
    assert_eq!(first.fraction, [0.00125, 1.0 / 1200.0]);
    assert_eq!(last.fraction, [799.0 / 800.0, 1199.0 / 1200.0]);
    assert!(FramePoint::new([400, 0], [400, 600]).is_err());
    assert!(FramePoint::new([0, 0], [0, 600]).is_err());
  }
}
