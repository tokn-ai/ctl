//! Screenshot-relative pointer actions and physical keyboard input for agents.

mod capture;
mod keyboard;
mod session;
pub use session::Automation;
#[cfg(test)]
mod tests;

use crate::{
  Session,
  input::Input,
  media::{Frame, VideoSource},
  transport::Transport,
  validate_window_id,
};
use capture::ActiveCapture;
use ctream_proto::{Target, WindowId};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Pixel position in the screenshot, with the origin at its top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Point {
  pub x: u32,
  pub y: u32,
}

impl Point {
  fn mapped(self, size: [u32; 2], native: [u32; 2]) -> io::Result<Self> {
    if self.x >= size[0] || self.y >= size[1] {
      return Err(invalid("pointer is outside the screenshot"));
    }
    // Align both endpoints, including the last pixel of a scaled X11 capture.
    let scale = |value: u32, from: u32, to: u32| {
      let span = u64::from(from - 1);
      let offset = u64::from(value) * u64::from(to - 1);
      ((offset + span / 2) / span.max(1)) as u32
    };
    Ok(Self {
      x: scale(self.x, size[0], native[0]),
      y: scale(self.y, size[1], native[1]),
    })
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
  Left = 1,
  Middle = 2,
  Right = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
  Up = 4,
  Down = 5,
  Left = 6,
  Right = 7,
}

impl<T: Transport> Session<T> {
  /// Check target scope and window identity syntax before contacting the host.
  pub fn validate_target(&self) -> io::Result<()> {
    if self.target == Target::Windows(0) {
      return Err(invalid("Windows display numbers start at 1"));
    }
    self.require_window_for_desktop()?;
    if let Some(window) = &self.window {
      validate_window_id(self.target, &window.id)?;
      if matches!(self.target, Target::X11(_)) && WindowId::x11_xid(&window.id).is_none() {
        return Err(invalid("invalid X11 window identifier"));
      }
    }
    Ok(())
  }

  /// Read the first RGB24 frame from a new capture of the selected source.
  /// On macOS, finish any open controls before requesting the next screenshot.
  pub fn screenshot(&self) -> io::Result<Frame> {
    self.validate_target()?;
    let mut capture = self.capture()?;
    let mut frame = Frame::default();
    capture.read_frame(&mut frame)?;
    if self.target == Target::MacosDesktop {
      ActiveCapture::start(capture)?.finish()?;
    }
    Ok(frame)
  }

  /// Open a persistent input channel for the selected source. On macOS the
  /// agent authorizes input against an active capture of the same window.
  pub fn controls(&self) -> io::Result<Controls<T::Reader, T::Writer>> {
    self.validate_target()?;
    let capture = if self.target == Target::MacosDesktop {
      Some(ActiveCapture::start(self.capture_with_driver("auto")?)?)
    } else {
      None
    };
    let input = self.input()?;
    Ok(Controls {
      input: Some(input),
      capture,
    })
  }
}

/// Input for one selected display or window. Pointer actions take the dimensions
/// of the screenshot used to choose their coordinates. A failed transport
/// write closes the input channel so the agent can release held input.
pub struct Controls<R: Read + Send + 'static, W: Write + Send + 'static> {
  // Release input before the capture that authorizes it on macOS.
  input: Option<Input<R, W>>,
  capture: Option<ActiveCapture<W>>,
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Controls<R, W> {
  /// Close input, wait for the agent to finish processing it, and release any
  /// matching window capture. Surfaces remote errors available at EOF.
  pub fn finish(mut self) -> io::Result<()> {
    let input = self.input.take().ok_or_else(closed)?.finish();
    let capture = self.capture.as_mut().map(ActiveCapture::finish).transpose();
    input.and(capture.map(|_| ()))
  }

  fn dimensions(&self, size: [u32; 2]) -> io::Result<[u32; 2]> {
    Ok(
      self
        .input
        .as_ref()
        .ok_or_else(closed)?
        .dimensions()
        .unwrap_or(size),
    )
  }

  fn send(&mut self, send: impl FnOnce(&mut Input<R, W>) -> io::Result<()>) -> io::Result<()> {
    let result = send(self.input.as_mut().ok_or_else(closed)?);
    if result.is_err() {
      drop(self.input.take());
      drop(self.capture.take());
    }
    result
  }

  pub fn move_to(&mut self, point: Point, size: [u32; 2]) -> io::Result<()> {
    let native = self.dimensions(size)?;
    let point = point.mapped(size, native)?;
    self.send(|input| input.move_pointer(point.x, point.y, native))
  }

  pub fn click(&mut self, point: Point, size: [u32; 2], button: MouseButton) -> io::Result<()> {
    self.move_to(point, size)?;
    self.send(|input| {
      input.button(button as u8, true)?;
      input.button(button as u8, false)
    })
  }

  pub fn drag(
    &mut self,
    from: Point,
    to: Point,
    size: [u32; 2],
    button: MouseButton,
  ) -> io::Result<()> {
    let native = self.dimensions(size)?;
    let from = from.mapped(size, native)?;
    let to = to.mapped(size, native)?;
    self.send(|input| {
      input.move_pointer(from.x, from.y, native)?;
      input.button(button as u8, true)?;
      input.move_pointer(to.x, to.y, native)?;
      input.button(button as u8, false)
    })
  }

  /// Move to the point and send 1 through 256 wheel steps.
  pub fn scroll(
    &mut self,
    point: Point,
    size: [u32; 2],
    direction: ScrollDirection,
    steps: u16,
  ) -> io::Result<()> {
    if !(1..=256).contains(&steps) {
      return Err(invalid("scroll requires 1 to 256 steps"));
    }
    self.move_to(point, size)?;
    self.send(|input| {
      for _ in 0..steps {
        input.button(direction as u8, true)?;
        input.button(direction as u8, false)?;
      }
      Ok(())
    })
  }

  /// Press a chord of 1 through 32 physical keys in order and release them
  /// in reverse order. Use ctream keysym names such as `Control_L` and `a`.
  /// The target driver determines which keys it can deliver.
  pub fn press_keys(&mut self, keys: &[String]) -> io::Result<()> {
    keyboard::validate_chord(keys)?;
    self.send(|input| {
      for key in keys {
        input.key(key, true)?;
      }
      for key in keys.iter().rev() {
        input.key(key, false)?;
      }
      Ok(())
    })
  }

  /// Type printable ASCII, tab, and line breaks as physical US-layout key
  /// presses. CRLF generates one Return. The target layout and Caps Lock
  /// state determine the text an application receives.
  pub fn type_text(&mut self, text: &str) -> io::Result<()> {
    if !text
      .bytes()
      .all(|byte| matches!(byte, b' '..=b'~' | b'\n' | b'\r' | b'\t'))
    {
      return Err(invalid(
        "physical text input supports printable US ASCII, tab, and line breaks",
      ));
    }
    self.send(|input| {
      let mut bytes = text.bytes().peekable();
      while let Some(byte) = bytes.next() {
        if byte == b'\r' {
          bytes.next_if_eq(&b'\n');
        }
        let (key, shifted) = keyboard::us_key(byte);
        if shifted {
          input.key("Shift_L", true)?;
        }
        input.key(key, true)?;
        input.key(key, false)?;
        if shifted {
          input.key("Shift_L", false)?;
        }
      }
      Ok(())
    })
  }
}

fn closed() -> io::Error {
  io::Error::new(io::ErrorKind::NotConnected, "input channel is closed")
}

impl<R: Read + Send + 'static, W: Write + Send + 'static> Drop for Controls<R, W> {
  fn drop(&mut self) {
    // Ending the macOS capture also ends its associated input. Let buffered
    // input reach its target first, even when the caller omits finish().
    if self.capture.is_some()
      && let Some(input) = self.input.take()
    {
      let _ = input.finish();
    }
  }
}
