//! Native capabilities used by the standalone agent.
//!
//! Provider sessions retain target identity. Each capability opens a resource
//! with its own lifetime, and only supported capabilities are implemented.

mod atspi;
mod process;
mod wayland;
mod x11;

pub use process::CaptureLifetime;
pub use wayland::Wayland;
pub use x11::X11;

use crate::protocol::Window;
use std::{env, fs, io::Write, os::unix::fs::MetadataExt};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn discover_sessions() -> Result<Vec<crate::protocol::Session>> {
  let runtime_uid = env::var_os("XDG_RUNTIME_DIR")
    .and_then(|path| fs::metadata(path).ok())
    .map(|metadata| metadata.uid());
  Ok(
    crate::discover()?
      .into_iter()
      .map(|(display, class)| {
        let selectable = match &display {
          crate::Display::X11(_) => true,
          crate::Display::Wayland { uid, .. } => runtime_uid == Some(*uid),
        };
        crate::protocol::Session {
          display,
          class,
          selectable,
        }
      })
      .collect(),
  )
}

pub trait Provider {
  type Session;

  fn open(&self, display: u32) -> Self::Session;
}

pub trait DiscoverWindows {
  fn windows(&self) -> Result<Vec<Window>>;
}

pub trait CaptureSource {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()>;
}

pub trait CaptureDisplay {
  type Source: CaptureSource;

  fn capture_display(&self) -> Result<Self::Source>;
}

pub trait CaptureWindow {
  type Source: CaptureSource;

  fn capture_window(&self, id: &str) -> Result<Self::Source>;
}

pub trait InputSource {
  fn run(self) -> Result<()>;
}

pub trait ForwardInput {
  type Input: InputSource;

  fn input(&self) -> Result<Self::Input>;
}

#[derive(Clone, Copy)]
pub enum ControlOperation {
  Inspect { bounds: bool },
  Act,
}

pub trait ControlSource {
  fn run(self, operation: ControlOperation) -> Result<()>;
}

pub trait AccessibleControls {
  type Controls: ControlSource;

  fn controls(&self, id: &str) -> Result<Self::Controls>;
}
