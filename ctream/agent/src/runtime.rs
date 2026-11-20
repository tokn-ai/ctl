//! Stdio hosting for drivers composed into an agent.

use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, Driver, ForwardInput, InputSource,
  Result,
};
use std::io;

pub fn capture_display<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: CaptureDisplay,
{
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  driver
    .open(target)?
    .capture_display()?
    .run(&mut io::stdout(), &lifetime)
}

pub fn capture_window<D: Driver>(driver: D, target: &D::Target, id: &str) -> Result<()>
where
  D::Session: CaptureWindow,
{
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  driver
    .open(target)?
    .capture_window(id)?
    .run(&mut io::stdout(), &lifetime)
}

pub fn forward_input<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: ForwardInput,
{
  driver.open(target)?.input()?.run()
}
