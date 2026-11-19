use super::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, DiscoverWindows, ForwardInput,
  Provider, Result,
  process::{InputProcess, ProcessCapture},
};
use crate::{
  protocol::{Window, X11_FRAME_SIZE, valid_window_id},
  windows,
};
use std::{
  io::{self, Write},
  process::{Command, Stdio},
};

pub struct X11;

pub struct Session {
  display: String,
}

impl Provider for X11 {
  type Session = Session;

  fn open(&self, display: u32) -> Session {
    Session {
      display: format!(":{display}"),
    }
  }
}

impl Session {
  fn helper(&self) -> Command {
    let mut command = Command::new("python3");
    command
      .args(["-u", "-c", include_str!("../x11_input.py")])
      .env("DISPLAY", &self.display);
    command
  }
}

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<Window>> {
    windows::x11::list(&self.display)
  }
}

impl CaptureDisplay for Session {
  type Source = ProcessCapture;

  fn capture_display(&self) -> Result<ProcessCapture> {
    let output = self
      .helper()
      .arg("--geometry")
      .stdin(Stdio::null())
      .output()?;
    let geometry = String::from_utf8_lossy(&output.stdout);
    let dimensions: Vec<_> = geometry
      .split_ascii_whitespace()
      .map(str::parse::<u32>)
      .collect();
    let [Ok(width), Ok(height)] = dimensions.as_slice() else {
      return Err(
        io::Error::other(format!(
          "cannot read X11 geometry for {}: {}",
          self.display,
          String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into(),
      );
    };
    if !output.status.success() || *width == 0 || *height == 0 {
      return Err(io::Error::other("invalid X11 geometry").into());
    }
    let [w, h] = X11_FRAME_SIZE;
    let mut command = Command::new("ffmpeg");
    command.args([
      "-nostdin",
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "x11grab",
      "-framerate",
      "8",
      "-video_size",
      &format!("{width}x{height}"),
      "-i",
      &self.display,
      "-vf",
      &format!("scale={w}:{h}"),
      "-pix_fmt",
      "rgb24",
      "-f",
      "rawvideo",
      "pipe:1",
    ]);
    Ok(ProcessCapture::stream(command))
  }
}

pub struct CompositeCapture {
  display: String,
  window: String,
}

impl CaptureSource for CompositeCapture {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    windows::x11::capture_to(&self.display, &self.window, output, |delay| {
      lifetime.cancelled(delay)
    })
  }
}

impl CaptureWindow for Session {
  type Source = CompositeCapture;

  fn capture_window(&self, id: &str) -> Result<CompositeCapture> {
    if !valid_window_id(id) {
      return Err("invalid window identifier".into());
    }
    Ok(CompositeCapture {
      display: self.display.clone(),
      window: id.to_owned(),
    })
  }
}

impl ForwardInput for Session {
  type Input = InputProcess;

  fn input(&self) -> Result<InputProcess> {
    Ok(InputProcess(self.helper()))
  }
}
