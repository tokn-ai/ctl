//! X11 display capture through ffmpeg, using the fixed RGB wire format.

use crate::process::ProcessCapture;
use ctream_driver::{CaptureDisplay, Result};
use ctream_proto::X11_FRAME_SIZE;
use std::{io, num::NonZeroUsize, process::Command, time::Duration};
use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

const FRAME_RATE: u32 = 8;

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
      &FRAME_RATE.to_string(),
      "-video_size",
      &format!("{width}x{height}"),
      "-i",
      &self.display,
      "-vf",
      &format!("scale={w}:{h}"),
      "-pix_fmt",
      "rgb24",
      // Keep each captured frame once. The rawvideo muxer's default sync
      // can synthesize duplicates at a higher rate than x11grab.
      "-vsync",
      "passthrough",
      "-f",
      "rawvideo",
      "pipe:1",
    ]);
    Ok(ProcessCapture::paced(
      command,
      NonZeroUsize::new(w * h * 3).expect("X11 RGB frames are nonempty"),
      Duration::from_secs(1) / FRAME_RATE,
    ))
  }
}
