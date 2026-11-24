//! Wayland display snapshots through grim, streamed as PPM frames.

use super::helper;
use crate::process::ProcessCapture;
use ctream_driver::{CaptureDisplay, CaptureOptions, Result};
use std::{process::Command, time::Duration};

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

impl Session {
  fn screenshot_command(&self) -> Command {
    let mut command = helper(self.display, "grim");
    command.args(["-t", "ppm", "-"]);
    command
  }
}

impl CaptureDisplay for Session {
  type Source = ProcessCapture;

  fn capture_display(&self) -> Result<ProcessCapture> {
    Ok(ProcessCapture::sampled(
      self.screenshot_command(),
      Duration::from_millis(125),
    ))
  }

  fn capture_display_with_options(&self, options: CaptureOptions) -> Result<ProcessCapture> {
    Ok(ProcessCapture::sampled_at_rate(
      self.screenshot_command(),
      options.interval(),
    ))
  }
}
