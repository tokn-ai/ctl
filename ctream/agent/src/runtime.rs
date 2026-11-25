//! Stdio hosting for drivers composed into an agent.

use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, Driver, ForwardInput,
  ForwardWindowInput, InputSource, Result,
};
use ctream_driver_codec::ffmpeg::Encoder;
use ctream_proto::{
  CAPTURE_STARTED_LIMIT, CaptureFormat, CaptureStarted, PING_HELLO, PING_TOKEN, VideoCapabilities,
  VideoCodec, VideoRequest, VideoStarted, valid_capture_driver,
};
use std::io::{self, Read, Write};

/// Reply to latency probes over stdio until the client closes the channel.
pub fn ping() -> Result<()> {
  echo_ping(io::stdin().lock(), io::stdout().lock())?;
  Ok(())
}

fn echo_ping(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
  output.write_all(&[PING_HELLO])?;
  output.flush()?;
  let mut token = [0];
  loop {
    match input.read_exact(&mut token) {
      Ok(()) if token == [PING_TOKEN] => {
        output.write_all(&token)?;
        output.flush()?;
      }
      Ok(()) => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "invalid ping token",
        ));
      }
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
      Err(error) => return Err(error),
    }
  }
}

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

pub fn capture_started<S: CaptureSource, W: Write + Send>(
  source: S,
  driver: &str,
  format: CaptureFormat,
  output: &mut W,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  validate_actual_driver(driver)?;
  let header = serde_json::to_vec(&CaptureStarted {
    driver: driver.into(),
    format,
  })?;
  source.run(&mut StartedWriter::new(output, header)?, lifetime)
}

/// Advertise input synchronization without opening a target or injecting input.
pub fn input_capabilities(output: impl Write) -> Result<()> {
  serde_json::to_writer(output, &ctream_proto::InputCapabilities { sync: true })?;
  Ok(())
}

/// Advertise supported codec negotiation. Encoder availability is checked when
/// a capture opens because FFmpeg and its encoders are platform dependencies.
pub fn video_capabilities(output: impl Write) -> Result<()> {
  serde_json::to_writer(
    output,
    &VideoCapabilities {
      codecs: vec![VideoCodec::Av1, VideoCodec::H264],
    },
  )?;
  Ok(())
}

pub fn capture_video_with_metadata<S: CaptureSource>(
  source: S,
  driver: &str,
  format: CaptureFormat,
  request: VideoRequest,
) -> Result<()> {
  let lifetime = CaptureLifetime::from_reader(io::stdin());
  capture_video_started(
    source,
    driver,
    format,
    request,
    &mut io::stdout(),
    &lifetime,
  )
}

/// Encode a capture source and announce the actual encoder and settings when
/// its first segment starts.
pub fn capture_video_started<S: CaptureSource, W: Write + Send>(
  source: S,
  driver: &str,
  format: CaptureFormat,
  request: VideoRequest,
  output: &mut W,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  validate_actual_driver(driver)?;
  request.validate()?;
  let encoder =
    Encoder::open_cancellable(request.options, &request.codecs, &lifetime.cancellation())?;
  let header = serde_json::to_vec(&VideoStarted {
    driver: driver.into(),
    video: encoder.info(),
  })?;
  encoder.run(
    format,
    &mut StartedWriter::new(output, header)?,
    lifetime,
    |pipe| source.run(pipe, lifetime),
  )
}

fn validate_actual_driver(driver: &str) -> Result<()> {
  if driver == "auto" || !valid_capture_driver(driver) {
    return Err("invalid actual capture driver identifier".into());
  }
  Ok(())
}

struct StartedWriter<'a, W> {
  output: &'a mut W,
  header: Option<Vec<u8>>,
}

impl<'a, W> StartedWriter<'a, W> {
  fn new(output: &'a mut W, mut header: Vec<u8>) -> Result<Self> {
    header.push(b'\n');
    if header.len() > CAPTURE_STARTED_LIMIT {
      return Err("capture startup metadata exceeds the limit".into());
    }
    Ok(Self {
      output,
      header: Some(header),
    })
  }
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

  #[test]
  fn ping_announces_startup_and_echoes_only_valid_tokens() {
    let mut output = Vec::new();
    echo_ping(&b"??"[..], &mut output).unwrap();
    assert_eq!(output, b"!??");

    let mut output = Vec::new();
    let error = echo_ping(&b"?x?"[..], &mut output).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(output, b"!?");
  }

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

  #[test]
  fn video_capabilities_report_concrete_codecs() {
    let mut output = Vec::new();
    video_capabilities(&mut output).unwrap();
    let capabilities: VideoCapabilities = serde_json::from_slice(&output).unwrap();
    assert_eq!(capabilities.codecs, [VideoCodec::Av1, VideoCodec::H264]);
  }

  #[test]
  fn invalid_video_options_have_no_startup_metadata() {
    let (_sender, receiver) = std::sync::mpsc::channel();
    let lifetime = CaptureLifetime::from_receiver(receiver);
    let mut output = Vec::new();
    let error = capture_video_started(
      Source { fail: false },
      "user.capture",
      CaptureFormat::Ppm,
      VideoRequest {
        options: ctream_proto::VideoOptions {
          max_fps: 0,
          ..Default::default()
        },
        codecs: vec![VideoCodec::H264],
      },
      &mut output,
      &lifetime,
    )
    .unwrap_err();
    assert!(error.to_string().contains("frame rate"));
    assert!(output.is_empty());
  }
}
