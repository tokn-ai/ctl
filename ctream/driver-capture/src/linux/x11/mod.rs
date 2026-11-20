//! X11 display capture through ffmpeg and window capture through Composite.

mod windows;

use crate::process::ProcessCapture;
use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureSource, CaptureWindow, DiscoverWindows, Result,
};
use ctream_proto::{Window, X11_FRAME_SIZE, valid_window_id};
use std::{
  io::{self, Write},
  process::Command,
};
use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

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

impl DiscoverWindows for Session {
  fn windows(&self) -> Result<Vec<Window>> {
    windows::list(&self.display)
  }
}

impl CaptureDisplay for Session {
  type Source = ProcessCapture;

  fn capture_display(&self) -> Result<ProcessCapture> {
    let (connection, screen) = x11rb::connect(Some(&self.display))?;
    let root = connection.setup().roots[screen].root;
    let geometry = connection.get_geometry(root)?.reply()?;
    let (width, height) = (geometry.width, geometry.height);
    if width == 0 || height == 0 {
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
    windows::capture_to(&self.display, &self.window, output, |delay| {
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
