use super::{
  AccessibleControls, CaptureDisplay, CaptureWindow, DiscoverWindows, ForwardInput, Provider,
  Result,
  atspi::AtspiWindow,
  process::{InputProcess, ProcessCapture},
};
use crate::{
  protocol::{Target, Window, valid_window_id},
  windows,
};
use std::{process::Command, time::Duration};

pub struct Wayland;

pub struct Session {
  display: u32,
}

impl Provider for Wayland {
  type Session = Session;

  fn open(&self, display: u32) -> Session {
    Session { display }
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
    windows::list(Target::Wayland(self.display))
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

impl ForwardInput for Session {
  type Input = InputProcess;

  fn input(&self) -> Result<InputProcess> {
    Ok(InputProcess(self.helper("ctream-wayland-input")))
  }
}

impl AccessibleControls for Session {
  type Controls = AtspiWindow;

  fn controls(&self, id: &str) -> Result<AtspiWindow> {
    if !valid_window_id(id) {
      return Err("invalid window identifier".into());
    }
    let window = self
      .windows()?
      .into_iter()
      .find(|window| window.id == id)
      .ok_or("selected window is no longer available")?;
    Ok(AtspiWindow::new(window))
  }
}
