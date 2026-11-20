//! Wayland display capture through grim and foreign-toplevel window capture.

use crate::process::ProcessCapture;
use ctream_driver::{CaptureDisplay, CaptureWindow, DiscoverWindows, Result};
use ctream_proto::{Window, valid_window_id};
use std::{
  process::{Command, Stdio},
  time::Duration,
};

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
  fn helper(&self, name: &str) -> Command {
    let mut command = Command::new(name);
    command
      .env_remove("WAYLAND_SOCKET")
      .env("WAYLAND_DISPLAY", format!("wayland-{}", self.display));
    command
  }
}

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<Window>> {
    let output = self
      .helper("ctream-wayland-window")
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

impl CaptureDisplay for Session {
  type Source = ProcessCapture;

  fn capture_display(&self) -> Result<ProcessCapture> {
    let mut command = self.helper("grim");
    command.args(["-t", "ppm", "-"]);
    Ok(ProcessCapture::sampled(command, Duration::from_millis(125)))
  }
}

impl CaptureWindow for Session {
  type Source = ProcessCapture;

  fn capture_window(&self, id: &str) -> Result<ProcessCapture> {
    if !valid_window_id(id) {
      return Err("invalid window identifier".into());
    }
    let mut command = self.helper("ctream-wayland-window");
    command.args(["capture", id]);
    Ok(ProcessCapture::stream(command))
  }
}
