//! Capability contracts shared by official and user-supplied ctream drivers.
//!
//! A driver opens a session for its own target type. Implement only the
//! capabilities that session supports; a capture driver need not implement input
//! or discovery. A user crate can expose separate `capture` and `input` modules
//! using these same contracts without depending on an official driver crate.
//!
//! These are Rust composition interfaces. The host application chooses drivers
//! and adapts their capabilities to its transport and supported wire targets.

mod lifetime;
mod pacing;

pub use lifetime::{CaptureCancellation, CaptureLifetime};
pub use pacing::FramePacer;

#[derive(Default)]
pub struct Frame<B = Vec<u8>> {
  pub size: [usize; 2],
  pub pixels: B,
}

use ctream_proto::Window;
use std::{
  io::{self, Write},
  time::Duration,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Source-side capture cadence. Timed providers default to their legacy 8 FPS;
/// native event-driven sources may use a different default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureOptions {
  max_fps: u16,
}

impl CaptureOptions {
  pub fn new(max_fps: u16) -> Result<Self> {
    if !(1..=120).contains(&max_fps) {
      return Err(
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "capture frame rate must be between 1 and 120 FPS",
        )
        .into(),
      );
    }
    Ok(Self { max_fps })
  }

  pub fn max_fps(self) -> u16 {
    self.max_fps
  }

  pub fn interval(self) -> Duration {
    Duration::from_secs(1) / u32::from(self.max_fps)
  }
}

impl Default for CaptureOptions {
  fn default() -> Self {
    Self { max_fps: 8 }
  }
}

/// Opens a session without imposing capabilities or a platform-specific target.
///
/// Associated targets may be owned structures or borrowed types such as `str`.
/// A session must own any target information it needs after `open` returns.
pub trait Driver {
  type Target: ?Sized;
  type Session;

  fn open(&self, target: &Self::Target) -> Result<Self::Session>;
}

pub trait DiscoverWindows {
  fn windows(&self) -> Result<Vec<Window>>;
}

/// An owned capture resource writing bytes in the format agreed with its host adapter.
pub trait CaptureSource {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()>;
}

pub trait CaptureDisplay {
  type Source: CaptureSource;

  fn capture_display(&self) -> Result<Self::Source>;

  /// Override to configure source cadence. Event-driven and custom sources may
  /// retain their native rate; the host also caps encoded frame delivery.
  fn capture_display_with_options(&self, _options: CaptureOptions) -> Result<Self::Source> {
    self.capture_display()
  }
}

pub trait CaptureWindow {
  type Source: CaptureSource;

  fn capture_window(&self, id: &str) -> Result<Self::Source>;

  fn capture_window_with_options(
    &self,
    id: &str,
    _options: CaptureOptions,
  ) -> Result<Self::Source> {
    self.capture_window(id)
  }
}

/// An input endpoint. Current helper adapters inherit the host's standard IO.
pub trait InputSource {
  fn run(self) -> Result<()>;

  /// Run the 1.1.14 input endpoint, including driver acknowledgements.
  fn run_synchronized(self) -> Result<()>
  where
    Self: Sized,
  {
    Err(
      io::Error::new(
        io::ErrorKind::Unsupported,
        "input synchronization is unavailable",
      )
      .into(),
    )
  }
}

pub trait ForwardInput {
  type Input: InputSource;

  fn input(&self) -> Result<Self::Input>;
}

/// Positional input scoped to an individual window selected within a session.
pub trait ForwardWindowInput {
  type Input: InputSource;

  fn input_window(&self, id: &str) -> Result<Self::Input>;
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
