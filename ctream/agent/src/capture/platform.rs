//! Adapt the bundled byte-stream capture drivers to complete shared frames.
use super::{Selection, pool::Frames};
use ctream_driver::{
  CaptureDisplay, CaptureLifetime, CaptureOptions, CaptureSource, CaptureWindow, Driver, Frame,
  Result,
};
use ctream_driver_codec::raw::{pixel_count, ppm_dimensions};
use ctream_proto::{CaptureFormat, Target};
use std::io::{self, Write};

struct FrameSink {
  publish: Frames,
  format: CaptureFormat,
  header: Vec<u8>,
  size: Option<[usize; 2]>,
  pixels: Vec<u8>,
}

impl FrameSink {
  fn new(publish: Frames, format: CaptureFormat) -> Self {
    Self {
      publish,
      format,
      header: Vec::with_capacity(64),
      size: match format {
        CaptureFormat::FixedRgb { size } => Some(size),
        CaptureFormat::Ppm => None,
      },
      pixels: Vec::new(),
    }
  }

  fn incomplete(&self) -> bool {
    !self.header.is_empty()
      || !self.pixels.is_empty()
      || (self.format == CaptureFormat::Ppm && self.size.is_some())
  }
}

impl Write for FrameSink {
  fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
    let length = bytes.len();
    while !bytes.is_empty() {
      let Some(size) = self.size else {
        let split = bytes
          .iter()
          .position(|b| *b == b'\n')
          .map_or(bytes.len(), |i| i + 1);
        if self.header.len() + split > 192 {
          return Err(io::Error::other("oversized capture frame header"));
        }
        self.header.extend_from_slice(&bytes[..split]);
        bytes = &bytes[split..];
        if self.header.iter().filter(|b| **b == b'\n').count() == 3 {
          self.size = Some(ppm_dimensions(&mut self.header.as_slice())?);
          self.header.clear();
        }
        continue;
      };
      let frame_bytes = pixel_count(size)? * 3;
      if self.pixels.is_empty() {
        self.pixels.reserve_exact(frame_bytes);
      }
      let take = bytes.len().min(frame_bytes - self.pixels.len());
      self.pixels.extend_from_slice(&bytes[..take]);
      bytes = &bytes[take..];
      if self.pixels.len() == frame_bytes {
        self.pixels = self.publish.publish(Frame {
          size,
          pixels: std::mem::take(&mut self.pixels),
        });
        self.pixels.clear();
        if self.format == CaptureFormat::Ppm {
          self.size = None;
        }
      }
    }
    Ok(length)
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

pub(crate) fn capture(
  target: Target,
  window: Option<&str>,
  provider: Selection,
  fps: Option<u16>,
  frames: Frames,
  lifetime: CaptureLifetime,
) -> io::Result<()> {
  let mut sink = FrameSink::new(frames, provider.format);
  let options = fps
    .map(CaptureOptions::new)
    .transpose()
    .map_err(|e| io::Error::other(e.to_string()))?;
  run_driver(target, provider.id, window, options, &mut sink, &lifetime)
    .map_err(|e| io::Error::other(e.to_string()))?;
  if sink.incomplete() && !lifetime.cancelled(std::time::Duration::ZERO) {
    return Err(io::Error::new(
      io::ErrorKind::UnexpectedEof,
      "capture ended during a frame",
    ));
  }
  Ok(())
}

fn display(
  source: impl CaptureDisplay,
  options: Option<CaptureOptions>,
  sink: &mut FrameSink,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  match options {
    Some(options) => source.capture_display_with_options(options)?,
    None => source.capture_display()?,
  }
  .run(sink, lifetime)
}

fn window(
  source: impl CaptureWindow,
  id: &str,
  options: Option<CaptureOptions>,
  sink: &mut FrameSink,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  match options {
    Some(options) => source.capture_window_with_options(id, options)?,
    None => source.capture_window(id)?,
  }
  .run(sink, lifetime)
}

#[cfg(target_os = "linux")]
fn run_driver(
  target: Target,
  driver: &str,
  id: Option<&str>,
  options: Option<CaptureOptions>,
  sink: &mut FrameSink,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  use ctream_driver_capture::linux::{wayland, x11};
  match (target, driver, id) {
    (Target::X11(n), "linux.x11.ffmpeg", None) => {
      display(x11::ffmpeg::Driver.open(&n)?, options, sink, lifetime)
    }
    (Target::X11(n), "linux.x11.composite", Some(id)) => window(
      x11::composite::Driver.open(&n)?,
      id,
      options,
      sink,
      lifetime,
    ),
    (Target::Wayland(n), "linux.wayland.grim", None) => {
      display(wayland::grim::Driver.open(&n)?, options, sink, lifetime)
    }
    (Target::Wayland(n), "linux.wayland.image_copy", Some(id)) => window(
      wayland::image_copy::Driver.open(&n)?,
      id,
      options,
      sink,
      lifetime,
    ),
    _ => Err("capture target requires a native desktop host".into()),
  }
}

#[cfg(windows)]
fn run_driver(
  target: Target,
  driver: &str,
  id: Option<&str>,
  options: Option<CaptureOptions>,
  sink: &mut FrameSink,
  lifetime: &CaptureLifetime,
) -> Result<()> {
  use ctream_driver_capture::windows::{dxgi, wgc};
  match (target, driver, id) {
    (Target::Windows(n), "windows.dxgi", None) => {
      display(dxgi::Driver.open(&n)?, options, sink, lifetime)
    }
    (Target::Windows(n), "windows.wgc", Some(id)) => {
      window(wgc::Driver.open(&n)?, id, options, sink, lifetime)
    }
    (Target::WindowsDesktop, "windows.wgc", Some(id)) => match options {
      Some(options) => wgc::WgcCapture::open_desktop_with_options(id, options)?,
      None => wgc::WgcCapture::open_desktop(id)?,
    }
    .run(sink, lifetime),
    _ => Err("capture target requires a native desktop host".into()),
  }
}
