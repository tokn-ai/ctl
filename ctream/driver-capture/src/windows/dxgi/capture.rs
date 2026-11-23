use super::super::{
  pixels::{self, Rotation},
  readback::TextureReader,
};
use super::Output;
use ctream_driver::{CaptureLifetime, CaptureSource, FramePacer, Result};
use ctream_windows::PhysicalPixels;
use std::{
  io::Write,
  time::{Duration, Instant},
};
use windows::{
  Win32::Graphics::{
    Direct3D11::ID3D11Texture2D,
    Dxgi::{
      Common::{
        DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180,
        DXGI_MODE_ROTATION_ROTATE270, DXGI_MODE_ROTATION_UNSPECIFIED,
      },
      DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutput1,
      IDXGIOutputDuplication,
    },
  },
  core::Interface,
};

const FRAME_WAIT_MS: u32 = 100;

struct AcquiredFrame<'a>(Option<&'a IDXGIOutputDuplication>);

impl AcquiredFrame<'_> {
  fn release(mut self) -> windows::core::Result<()> {
    unsafe { self.0.take().expect("acquired frame").ReleaseFrame() }
  }
}

impl Drop for AcquiredFrame<'_> {
  fn drop(&mut self) {
    if let Some(duplication) = self.0.take() {
      let _ = unsafe { duplication.ReleaseFrame() };
    }
  }
}

/// An owned display duplicator streaming physical-resolution P6 RGB frames.
pub struct DxgiCapture {
  reader: TextureReader,
  duplication: IDXGIOutputDuplication,
  rotation: Rotation,
  size: [usize; 2],
  frame_interval: Duration,
}

impl DxgiCapture {
  pub(super) fn new(output: &Output, frame_interval: Duration) -> Result<Self> {
    let _physical = PhysicalPixels::enter()?;
    let reader = TextureReader::new(Some(&output.adapter))?;
    let duplication = unsafe {
      output
        .output
        .cast::<IDXGIOutput1>()?
        .DuplicateOutput(reader.device())
    }
    .map_err(|error| {
      format!("Windows display capture is unavailable in this desktop session: {error}")
    })?;
    let rotation = match output.description.Rotation {
      DXGI_MODE_ROTATION_UNSPECIFIED | DXGI_MODE_ROTATION_IDENTITY => Rotation::Identity,
      DXGI_MODE_ROTATION_ROTATE90 => Rotation::Clockwise90,
      DXGI_MODE_ROTATION_ROTATE180 => Rotation::Clockwise180,
      DXGI_MODE_ROTATION_ROTATE270 => Rotation::Clockwise270,
      _ => return Err("unsupported Windows display rotation".into()),
    };
    let [left, top, right, bottom] = output.info.bounds;
    let width = usize::try_from(i64::from(right) - i64::from(left))?;
    let height = usize::try_from(i64::from(bottom) - i64::from(top))?;
    let size = pixels::output_size([width, height], Rotation::Identity)?;
    Ok(Self {
      reader,
      duplication,
      rotation,
      size,
      frame_interval,
    })
  }
}

impl CaptureSource for DxgiCapture {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let _physical = PhysicalPixels::enter()?;
    let mut next_frame = Instant::now();
    let mut pacer = FramePacer::new(self.frame_interval);
    while !lifetime.cancelled(next_frame.saturating_duration_since(Instant::now())) {
      let started = Instant::now();
      let mut information = DXGI_OUTDUPL_FRAME_INFO::default();
      let mut resource = None;
      match unsafe {
        self
          .duplication
          .AcquireNextFrame(FRAME_WAIT_MS, &mut information, &mut resource)
      } {
        Ok(()) => {}
        Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
        Err(error) if error.code() == DXGI_ERROR_ACCESS_LOST => {
          return Err("Windows desktop access changed; reconnect the capture".into());
        }
        Err(error) => return Err(error.into()),
      }
      let duplication = self.duplication.clone();
      let frame = AcquiredFrame(Some(&duplication));
      let texture = resource
        .ok_or("DXGI returned no desktop frame")?
        .cast::<ID3D11Texture2D>()?;
      let pixels = self.reader.read(&texture, self.size, self.rotation)?;
      frame.release()?;
      if lifetime.cancelled(Duration::ZERO) {
        break;
      }
      let [width, height] = self.size;
      write!(output, "P6\n{width} {height}\n255\n")?;
      output.write_all(pixels)?;
      output.flush()?;
      next_frame = pacer.finish_frame(started, Instant::now());
    }
    Ok(())
  }
}
