//! Wayland display snapshots through grim, streamed as PPM frames.

use super::helper;
use crate::process::ProcessCapture;
use ctream_driver::{CaptureDisplay, Result};
use std::time::Duration;

pub struct Driver;

pub struct Session {
  display: u32,
}

impl ctream_driver::Driver for Driver {
  type Target = u32;
  type Session = Session;

  fn open(&self, display: &u32) -> Result<Session> {
    Ok(Session { display: *display })
  }
}

impl CaptureDisplay for Session {
  type Source = ProcessCapture;

  fn capture_display(&self) -> Result<ProcessCapture> {
    let mut command = helper(self.display, "grim");
    command.args(["-t", "ppm", "-"]);
    Ok(ProcessCapture::sampled(command, Duration::from_millis(125)))
  }
}
