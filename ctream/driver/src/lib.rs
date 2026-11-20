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

pub use lifetime::CaptureLifetime;

use ctream_proto::Window;
use std::io::Write;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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
}

pub trait CaptureWindow {
  type Source: CaptureSource;

  fn capture_window(&self, id: &str) -> Result<Self::Source>;
}

/// An input endpoint. Current helper adapters inherit the host's standard IO.
pub trait InputSource {
  fn run(self) -> Result<()>;
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
