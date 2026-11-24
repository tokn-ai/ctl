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

fn accepts_passthrough(option: &str) -> io::Result<bool> {
  // Probe the flag without opening an input or output.
  Command::new("ffmpeg")
    .args(["-hide_banner", option, "passthrough", "-version"])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|status| status.success())
}

fn capture_filter([width, height]: [usize; 2], frame_rate: u16) -> String {
  // The raw pipe carries pixels and the host paces delivery. Number the
  // captured frames so X11 clock jitter cannot create equal or backward DTS.
  format!("scale={width}:{height},settb=1/{frame_rate},setpts=N")
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
    let frame_sync = frame_sync_option(accepts_passthrough)?;
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
      &capture_filter(X11_FRAME_SIZE, options.max_fps()),
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
  use std::{collections::HashSet, process::Output};

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

  #[test]
  fn indexes_frames_with_equal_or_backward_capture_timestamps() {
    if !Command::new("ffmpeg")
      .arg("-version")
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .status()
      .is_ok_and(|status| status.success())
    {
      eprintln!("FFmpeg not installed");
      return;
    }

    const SIZE: [usize; 2] = [32, 16];
    const FRAMES: usize = 6;
    let [width, height] = SIZE;
    let render = |frame_rate, filter: &str, sync| -> Output {
      Command::new("ffmpeg")
        .args([
          "-nostdin",
          "-hide_banner",
          "-loglevel",
          "error",
          "-f",
          "lavfi",
          "-i",
          &format!("testsrc=size={width}x{height}:rate={frame_rate},trim=end_frame={FRAMES}"),
          "-vf",
          filter,
          "-pix_fmt",
          "rgb24",
          sync,
          "passthrough",
          "-f",
          "rawvideo",
          "pipe:1",
        ])
        .output()
        .unwrap()
    };

    let sync = frame_sync_option(accepts_passthrough).unwrap();
    let mut cases = vec![(30, sync), (8, sync)];
    if sync == "-fps_mode" && accepts_passthrough("-vsync").unwrap() {
      cases.push((8, "-vsync"));
    }
    for (frame_rate, sync) in cases {
      let filter = capture_filter(SIZE, frame_rate);
      let original = render(frame_rate, &format!("scale={width}:{height}"), sync);
      // Present the original pictures at timestamps 3, 3, 4, 4, 2, 2.
      let colliding = render(
        frame_rate,
        &format!("setpts=3+floor(N/2)-3*floor(N/4),{filter}"),
        sync,
      );
      for output in [&original, &colliding] {
        assert!(
          output.status.success() && output.stderr.is_empty(),
          "{frame_rate} FPS {sync}: {}",
          String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout.len(), FRAMES * width * height * 3);
      }
      assert_eq!(
        original
          .stdout
          .chunks_exact(width * height * 3)
          .collect::<HashSet<_>>()
          .len(),
        FRAMES,
        "fixture produces distinct pictures"
      );
      assert!(
        colliding.stdout == original.stdout,
        "{frame_rate} FPS {sync} changed or reordered the captured pixels"
      );
    }
  }
}
