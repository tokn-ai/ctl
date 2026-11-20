use super::{
  Output,
  dpi::PhysicalPixels,
  pixels::{self, Rotation},
};
use ctream_driver::{CaptureLifetime, CaptureSource, Result};
use std::{
  io::Write,
  time::{Duration, Instant},
};
use windows::{
  Win32::{
    Foundation::HMODULE,
    Graphics::{
      Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
      Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
      },
      Dxgi::{
        Common::{
          DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_ROTATE90,
          DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
          DXGI_MODE_ROTATION_UNSPECIFIED,
        },
        DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutput1,
        IDXGIOutputDuplication,
      },
    },
  },
  core::Interface,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(125);
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

struct MappedTexture<'a>(&'a ID3D11DeviceContext, &'a ID3D11Texture2D);

impl Drop for MappedTexture<'_> {
  fn drop(&mut self) {
    unsafe { self.0.Unmap(self.1, 0) };
  }
}

/// An owned display duplicator streaming physical-resolution P6 RGB frames.
pub struct DxgiCapture {
  device: ID3D11Device,
  context: ID3D11DeviceContext,
  duplication: IDXGIOutputDuplication,
  rotation: Rotation,
  size: [usize; 2],
  staging: Option<ID3D11Texture2D>,
  staging_size: [usize; 2],
  pixels: Vec<u8>,
}

impl DxgiCapture {
  pub(super) fn new(output: &Output) -> Result<Self> {
    let _physical = PhysicalPixels::enter()?;
    let mut device = None;
    let mut context = None;
    unsafe {
      D3D11CreateDevice(
        &output.adapter,
        D3D_DRIVER_TYPE_UNKNOWN,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
      )?;
    }
    let device = device.ok_or("D3D11 returned no capture device")?;
    let context = context.ok_or("D3D11 returned no capture context")?;
    let duplication = unsafe {
      output
        .output
        .cast::<IDXGIOutput1>()?
        .DuplicateOutput(&device)
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
      device,
      context,
      duplication,
      rotation,
      size,
      staging: None,
      staging_size: [0, 0],
      pixels: Vec::new(),
    })
  }

  fn readback(&mut self, texture: &ID3D11Texture2D) -> Result<()> {
    let mut description = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut description) };
    let size = [description.Width as usize, description.Height as usize];
    if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM
      || description.SampleDesc.Count != 1
      || description.MipLevels != 1
      || description.ArraySize != 1
    {
      return Err("unsupported Windows capture texture format".into());
    }
    if pixels::output_size(size, self.rotation)? != self.size {
      return Err("Windows display geometry changed; reconnect the capture".into());
    }
    if self.staging.is_none() || self.staging_size != size {
      let staging = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..description
      };
      unsafe {
        self
          .device
          .CreateTexture2D(&staging, None, Some(&mut self.staging))
      }?;
      self.staging_size = size;
    }
    let staging = self
      .staging
      .as_ref()
      .ok_or("D3D11 returned no staging texture")?;
    unsafe { self.context.CopyResource(staging, texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
      self
        .context
        .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }?;
    let _mapping = MappedTexture(&self.context, staging);
    let pitch = mapped.RowPitch as usize;
    pixels::mapped_length(size, pitch)?;
    if mapped.pData.is_null() {
      return Err("invalid Windows capture memory layout".into());
    }
    // Map guarantees Width initialized BGRA pixels per row. Exclude row padding
    // from the slices because D3D does not promise initialized padding bytes.
    let rows: Vec<_> = (0..size[1])
      .map(|y| unsafe {
        std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(y * pitch), size[0] * 4)
      })
      .collect();
    pixels::convert_rows(&rows, size, self.rotation, &mut self.pixels)?;
    Ok(())
  }
}

impl CaptureSource for DxgiCapture {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let _physical = PhysicalPixels::enter()?;
    let mut next_frame = Instant::now();
    while !lifetime.cancelled(next_frame.saturating_duration_since(Instant::now())) {
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
      next_frame = Instant::now() + FRAME_INTERVAL;
      // Clone the COM handle so the guard does not borrow self during readback.
      let duplication = self.duplication.clone();
      let frame = AcquiredFrame(Some(&duplication));
      let texture = resource
        .ok_or("DXGI returned no desktop frame")?
        .cast::<ID3D11Texture2D>()?;
      self.readback(&texture)?;
      frame.release()?;
      if lifetime.cancelled(Duration::ZERO) {
        break;
      }
      let [width, height] = self.size;
      write!(output, "P6\n{width} {height}\n255\n")?;
      output.write_all(&self.pixels)?;
      output.flush()?;
    }
    Ok(())
  }
}
