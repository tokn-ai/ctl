use crate::process::InputProcess;
use ctream_driver::{ForwardInput, ForwardWindowInput, Result};
use std::process::Command;

mod window;
pub use window::WindowInput;

/// X11 display input through XTEST and window input through SendEvent.
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

impl ForwardInput for Session {
  type Input = InputProcess;

  fn input(&self) -> Result<InputProcess> {
    let mut command = Command::new("python3");
    command
      .args(["-u", "-c", include_str!("x11_input.py")])
      .env("DISPLAY", &self.display);
    Ok(InputProcess(command))
  }
}

impl ForwardWindowInput for Session {
  type Input = WindowInput;

  fn input_window(&self, id: &str) -> Result<WindowInput> {
    WindowInput::open(&self.display, id)
  }
}
