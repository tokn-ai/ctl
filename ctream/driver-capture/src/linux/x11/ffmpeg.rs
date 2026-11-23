//! X11 display capture through ffmpeg, using the fixed RGB wire format.

use crate::process::ProcessCapture;
use ctream_driver::{CaptureDisplay, CaptureOptions, Result};
use ctream_proto::X11_FRAME_SIZE;
use std::{
  io,
  num::NonZeroUsize,
  process::{Command, Stdio},
};
use x11rb::{connection::Connection, protocol::xproto::ConnectionExt};

fn frame_sync_option(
  mut accepts: impl FnMut(&str) -> io::Result<bool>,
) -> io::Result<&'static str> {
  for option in ["-fps_mode", "-vsync"] {
    if accepts(option)? {
      return Ok(option);
    }
  }
  Err(io::Error::new(
    io::ErrorKind::Unsupported,
    "FFmpeg does not accept a passthrough frame sync option",
  ))
}

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
    self.capture_display_with_options(CaptureOptions::default())
  }

  fn capture_display_with_options(&self, options: CaptureOptions) -> Result<ProcessCapture> {
    let (connection, screen) = x11rb::connect(Some(&self.display))?;
    let root = connection.setup().roots[screen].root;
    let geometry = connection.get_geometry(root)?.reply()?;
    let (width, height) = (geometry.width, geometry.height);
    if width == 0 || height == 0 {
      return Err(io::Error::other("invalid X11 geometry").into());
    }
    let [w, h] = X11_FRAME_SIZE;
    // FFmpeg checks whether it recognizes the flag before -version exits,
    // without opening an input or output.
    let frame_sync = frame_sync_option(|option| {
      Command::new("ffmpeg")
        .args(["-hide_banner", option, "passthrough", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
    })?;
    let mut command = Command::new("ffmpeg");
    command.args([
      "-nostdin",
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "x11grab",
      "-framerate",
      &options.max_fps().to_string(),
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
      frame_sync,
      "passthrough",
      "-f",
      "rawvideo",
      "pipe:1",
    ]);
    Ok(ProcessCapture::paced(
      command,
      NonZeroUsize::new(w * h * 3).expect("X11 RGB frames are nonempty"),
      options.interval(),
    ))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn selects_modern_frame_sync_and_falls_back_for_older_ffmpeg() {
    let mut tried = Vec::new();
    let modern = frame_sync_option(|option| {
      tried.push(option.to_owned());
      Ok(option == "-fps_mode")
    })
    .unwrap();
    assert_eq!(modern, "-fps_mode");
    assert_eq!(tried, ["-fps_mode"]);

    let legacy = frame_sync_option(|option| Ok(option == "-vsync")).unwrap();
    assert_eq!(legacy, "-vsync");
    assert_eq!(
      frame_sync_option(|_| Ok(false)).unwrap_err().kind(),
      io::ErrorKind::Unsupported
    );
  }
}
