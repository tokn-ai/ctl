use super::pixels::{self, Rotation};
use ctream_driver::Result;
use windows::Win32::{
  Foundation::HMODULE,
  Graphics::{
    Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN},
    Direct3D11::{
      D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
      D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
      D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    },
    Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIAdapter, IDXGIAdapter1},
  },
};
use windows::core::Interface;

struct MappedTexture<'a>(&'a ID3D11DeviceContext, &'a ID3D11Texture2D);

pub(super) enum WindowReadback<'a> {
  Complete(&'a [u8]),
  Clipped([usize; 2]),
}

impl Drop for MappedTexture<'_> {
  fn drop(&mut self) {
    unsafe { self.0.Unmap(self.1, 0) };
  }
}

/// Shared D3D staging readback. No mapped GPU memory escapes this type.
pub(super) struct TextureReader {
  device: ID3D11Device,
  context: ID3D11DeviceContext,
  staging: Option<ID3D11Texture2D>,
  staging_size: [usize; 2],
  pixels: Vec<u8>,
}

impl TextureReader {
  pub fn new(adapter: Option<&IDXGIAdapter1>) -> Result<Self> {
    let driver_type = if adapter.is_some() {
      D3D_DRIVER_TYPE_UNKNOWN
    } else {
      D3D_DRIVER_TYPE_HARDWARE
    };
    let adapter = adapter
      .map(|adapter| adapter.cast::<IDXGIAdapter>())
      .transpose()?;
    let mut device = None;
    let mut context = None;
    unsafe {
      D3D11CreateDevice(
        adapter.as_ref(),
        driver_type,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
      )
    }?;
    Ok(Self {
      device: device.ok_or("D3D11 returned no capture device")?,
      context: context.ok_or("D3D11 returned no capture context")?,
      staging: None,
      staging_size: [0, 0],
      pixels: Vec::new(),
    })
  }

  pub fn device(&self) -> &ID3D11Device {
    &self.device
  }

  pub fn enable_thread_protection(&self) -> Result<()> {
    // WGC's free-threaded producer shares this D3D device. Serialize immediate
    // context calls before handing the device to its worker thread.
    let threading = self.context.cast::<ID3D11Multithread>()?;
    let _ = unsafe { threading.SetMultithreadProtected(true) };
    Ok(())
  }

  pub fn read(
    &mut self,
    texture: &ID3D11Texture2D,
    expected: [usize; 2],
    rotation: Rotation,
  ) -> Result<&[u8]> {
    let description = texture_description(texture)?;
    let size = [description.Width as usize, description.Height as usize];
    if pixels::output_size(size, rotation)? != expected {
      return Err(
        format!(
          "Windows capture texture geometry changed unexpectedly: texture {}x{}, expected {}x{}",
          size[0], size[1], expected[0], expected[1]
        )
        .into(),
      );
    }
    self.read_region(texture, description, size, rotation)
  }

  pub fn read_window(
    &mut self,
    texture: &ID3D11Texture2D,
    content: [usize; 2],
  ) -> Result<WindowReadback<'_>> {
    let description = texture_description(texture)?;
    let size = [description.Width as usize, description.Height as usize];
    let Some(region) = pixels::window_content_size(size, content)? else {
      return Ok(WindowReadback::Clipped(size));
    };
    Ok(WindowReadback::Complete(self.read_region(
      texture,
      description,
      region,
      Rotation::Identity,
    )?))
  }

  fn read_region(
    &mut self,
    texture: &ID3D11Texture2D,
    description: D3D11_TEXTURE2D_DESC,
    size: [usize; 2],
    rotation: Rotation,
  ) -> Result<&[u8]> {
    if self.staging.is_none() || self.staging_size != size {
      let staging = D3D11_TEXTURE2D_DESC {
        Width: size[0] as u32,
        Height: size[1] as u32,
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..description
      };
      // An out-parameter must be empty: replacing an existing COM pointer through
      // its ABI would leak the old texture on every window resize.
      let mut replacement = None;
      unsafe {
        self
          .device
          .CreateTexture2D(&staging, None, Some(&mut replacement))
      }?;
      self.staging = replacement;
      self.staging_size = size;
    }
    let staging = self
      .staging
      .as_ref()
      .ok_or("D3D11 returned no staging texture")?;
    let region = D3D11_BOX {
      left: 0,
      top: 0,
      front: 0,
      right: size[0] as u32,
      bottom: size[1] as u32,
      back: 1,
    };
    // ContentSize, rather than the backing texture's dimensions, defines the
    // initialized rectangle. Copy exactly that rectangle into exact-size staging.
    unsafe {
      self
        .context
        .CopySubresourceRegion(staging, 0, 0, 0, 0, texture, 0, Some(&region))
    };
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
    // Only initialized BGRA pixels become slices; GPU row padding is undefined.
    let rows: Vec<_> = (0..size[1])
      .map(|y| unsafe {
        std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(y * pitch), size[0] * 4)
      })
      .collect();
    pixels::convert_rows(&rows, size, rotation, &mut self.pixels)?;
    Ok(&self.pixels)
  }
}

fn texture_description(texture: &ID3D11Texture2D) -> Result<D3D11_TEXTURE2D_DESC> {
  let mut description = D3D11_TEXTURE2D_DESC::default();
  unsafe { texture.GetDesc(&mut description) };
  if description.Format != DXGI_FORMAT_B8G8R8A8_UNORM
    || description.SampleDesc.Count != 1
    || description.MipLevels != 1
    || description.ArraySize != 1
  {
    return Err("unsupported Windows capture texture format".into());
  }
  Ok(description)
}
