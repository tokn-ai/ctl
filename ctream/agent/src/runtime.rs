//! Stdio hosting for drivers composed into an agent.

use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, Driver, ForwardInput,
  ForwardWindowInput, InputSource, Result,
};
use ctream_proto::{CAPTURE_STARTED_LIMIT, CaptureFormat, CaptureStarted, valid_capture_driver};
use std::io::{self, Write};

/// Announce an initialized capture before its first frame bytes. Sources may
/// acquire platform resources lazily in `run`, so constructor success alone is
/// insufficient to send a successful startup handshake.
pub fn capture_with_metadata<S: CaptureSource>(
  source: S,
  driver: &str,
  format: CaptureFormat,
) -> Result<()> {
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  capture_started(source, driver, format, &mut io::stdout(), &lifetime)
}

fn capture_started<S: CaptureSource, W: Write + Send>(
  source: S,
  driver: &str,
  format: CaptureFormat,
  output: &mut W,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  if driver == "auto" || !valid_capture_driver(driver) {
    return Err("invalid actual capture driver identifier".into());
  }
  let mut header = serde_json::to_vec(&CaptureStarted {
    driver: driver.into(),
    format,
  })?;
  header.push(b'\n');
  if header.len() > CAPTURE_STARTED_LIMIT {
    return Err("capture startup metadata exceeds the limit".into());
  }
  source.run(
    &mut StartedWriter {
      output,
      header: Some(header),
    },
    lifetime,
  )
}

struct StartedWriter<'a, W> {
  output: &'a mut W,
  header: Option<Vec<u8>>,
}

impl<W: Write> Write for StartedWriter<'_, W> {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
      return Ok(0);
    }
    if let Some(header) = self.header.take() {
      self.output.write_all(&header)?;
    }
    self.output.write(bytes)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.output.flush()
  }
}

pub fn capture_display<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: CaptureDisplay,
{
  stream_capture(driver.open(target)?.capture_display()?)
}

pub fn capture_window<D: Driver>(driver: D, target: &D::Target, id: &str) -> Result<()>
where
  D::Session: CaptureWindow,
{
  stream_capture(driver.open(target)?.capture_window(id)?)
}

pub fn stream_capture(source: impl CaptureSource) -> Result<()> {
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  source.run(&mut io::stdout(), &lifetime)
}

pub fn forward_input<D: Driver>(driver: D, target: &D::Target) -> Result<()>
where
  D::Session: ForwardInput,
{
  driver.open(target)?.input()?.run()
}

pub fn forward_window_input<D: Driver>(driver: D, target: &D::Target, id: &str) -> Result<()>
where
  D::Session: ForwardWindowInput,
{
  driver.open(target)?.input_window(id)?.run()
}

#[cfg(test)]
mod tests {
  use super::*;
  struct Source {
    fail: bool,
  }
  impl CaptureSource for Source {
    fn run<W: Write + Send>(self, output: &mut W, _: &CaptureLifetime) -> Result<()> {
      output.flush()?;
      if self.fail {
        return Err("provider initialization failed".into());
      }
      output.write_all(b"P6\n1 1\n255\nabc")?;
      Ok(())
    }
  }

  #[test]
  fn metadata_waits_for_source_start_and_precedes_frame_bytes() {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let lifetime = CaptureLifetime::from_receiver(receiver);
    let mut output = Vec::new();
    assert!(
      capture_started(
        Source { fail: true },
        "user.capture",
        CaptureFormat::Ppm,
        &mut output,
        &lifetime
      )
      .is_err()
    );
    assert!(output.is_empty());
    capture_started(
      Source { fail: false },
      "user.capture",
      CaptureFormat::Ppm,
      &mut output,
      &lifetime,
    )
    .unwrap();
    let newline = output.iter().position(|byte| *byte == b'\n').unwrap();
    let started: CaptureStarted = serde_json::from_slice(&output[..newline]).unwrap();
    assert_eq!(started.driver, "user.capture");
    assert_eq!(started.format, CaptureFormat::Ppm);
    assert_eq!(&output[newline + 1..], b"P6\n1 1\n255\nabc");
  }
}
