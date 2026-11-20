//! X11 window discovery and capture through the Composite extension.

use super::windows;
use ctream_driver::{CaptureLifetime, CaptureSource, CaptureWindow, DiscoverWindows, Result};
use ctream_proto::{Window, valid_window_id};
use std::io::Write;

pub struct Driver;

pub struct Session {
  display: String,
}

impl ctream_driver::Driver for Driver {
  type Target = u32;
  type Session = Session;

  fn open(&self, display: &u32) -> Result<Session> {
    Ok(Session {
      display: format!(":{display}"),
    })
  }
}

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<Window>> {
    windows::list(&self.display)
  }
}

pub struct CompositeCapture {
  display: String,
  window: String,
}

impl CaptureSource for CompositeCapture {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    windows::capture_to(&self.display, &self.window, output, |delay| {
      lifetime.cancelled(delay)
    })
  }
}

impl CaptureWindow for Session {
  type Source = CompositeCapture;

  fn capture_window(&self, id: &str) -> Result<CompositeCapture> {
    if !valid_window_id(id) {
      return Err("invalid window identifier".into());
    }
    Ok(CompositeCapture {
      display: self.display.clone(),
      window: id.to_owned(),
    })
  }
}
