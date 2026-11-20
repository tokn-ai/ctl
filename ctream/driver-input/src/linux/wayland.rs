use crate::process::InputProcess;
use ctream_driver::{ForwardInput, Result};
use std::process::Command;

/// Wayland input forwarding through the virtual input helper.
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

impl ForwardInput for Session {
  type Input = InputProcess;

  fn input(&self) -> Result<InputProcess> {
    let mut command = Command::new("ctream-wayland-input");
    command
      .env_remove("WAYLAND_SOCKET")
      .env("WAYLAND_DISPLAY", format!("wayland-{}", self.display));
    Ok(InputProcess(command))
  }
}
