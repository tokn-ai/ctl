//! Foreign-toplevel discovery and window capture through image-copy capture.
//!
//! The compositor completes one frame request at a time and can wait for the
//! contents to change. The encoder enforces the outgoing stream's frame cap.

use super::helper;
use crate::process::ProcessCapture;
use ctream_driver::{CaptureWindow, DiscoverWindows, Result};
use ctream_proto::{Window, valid_window_id};
use std::process::Stdio;

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

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<Window>> {
    let output = helper(self.display, "ctream-wayland-window")
      .arg("list")
      .stdin(Stdio::null())
      .output()?;
    if !output.status.success() {
      return Err(
        format!(
          "window discovery failed: {}",
          String::from_utf8_lossy(&output.stderr).trim()
        )
        .into(),
      );
    }
    let mut windows: Vec<Window> = serde_json::from_slice(&output.stdout)?;
    windows.retain(|window| valid_window_id(&window.id));
    Ok(windows)
  }
}

impl CaptureWindow for Session {
  type Source = ProcessCapture;

  fn capture_window(&self, id: &str) -> Result<ProcessCapture> {
    if !valid_window_id(id) {
      return Err("invalid window identifier".into());
    }
    let mut command = helper(self.display, "ctream-wayland-window");
    command.args(["capture", id]);
    Ok(ProcessCapture::stream(command))
  }
}
