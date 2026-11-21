//! Captures selected window surfaces with Windows Graphics Capture.

use super::{
  pixels::{self, Rotation},
  readback::{TextureReader, WindowReadback},
};
use ctream_driver::{CaptureLifetime, CaptureSource, CaptureWindow, Result};
use ctream_windows::{PhysicalPixels, WindowTarget};
use std::{
  io::Write,
  marker::PhantomData,
  rc::Rc,
  sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
  },
  time::{Duration, Instant},
};
use windows::{
  Foundation::TypedEventHandler,
  Graphics::{
    Capture::{
      Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
      GraphicsCaptureSession,
    },
    DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    SizeInt32,
  },
  Win32::{
    Graphics::{Direct3D11::ID3D11Texture2D, Dxgi::IDXGIDevice},
    System::WinRT::{
      Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
      Graphics::Capture::IGraphicsCaptureItemInterop,
      RO_INIT_MULTITHREADED, RoGetActivationFactory, RoInitialize, RoUninitialize,
    },
  },
  core::{HSTRING, Interface},
};

const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const FRAME_INTERVAL: Duration = Duration::from_millis(125);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const FRAME_RECOVERY: Duration = Duration::from_secs(10);

pub struct Driver;

/// A Win32 display scope used to select a window, independent of DXGI outputs.
pub struct Session {
  display: u32,
}

impl ctream_driver::Driver for Driver {
  type Target = u32;
  type Session = Session;

  fn open(&self, display: &u32) -> Result<Session> {
    ctream_windows::display(*display)?;
    Ok(Session { display: *display })
  }
}

impl CaptureWindow for Session {
  type Source = WgcCapture;

  fn capture_window(&self, id: &str) -> Result<WgcCapture> {
    Ok(WgcCapture {
      target: WindowTarget::open(self.display, id)?,
    })
  }
}

struct Runtime(PhantomData<Rc<()>>);

impl Runtime {
  fn enter() -> Result<Self> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED) }?;
    Ok(Self(PhantomData))
  }
}

impl Drop for Runtime {
  fn drop(&mut self) {
    unsafe { RoUninitialize() };
  }
}

struct CaptureItem {
  item: GraphicsCaptureItem,
  token: i64,
  closed: Arc<AtomicBool>,
}

impl CaptureItem {
  fn new(target: &WindowTarget) -> Result<Self> {
    let interop: IGraphicsCaptureItemInterop = unsafe {
      RoGetActivationFactory(&HSTRING::from(
        "Windows.Graphics.Capture.GraphicsCaptureItem",
      ))
    }?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(target.hwnd()) }?;
    let closed = Arc::new(AtomicBool::new(false));
    let notification = closed.clone();
    let token = item.Closed(&TypedEventHandler::new(move |_, _| {
      notification.store(true, Ordering::Release);
      Ok(())
    }))?;
    Ok(Self {
      item,
      token,
      closed,
    })
  }

  fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Acquire)
  }
}

impl Drop for CaptureItem {
  fn drop(&mut self) {
    let _ = self.item.RemoveClosed(self.token);
  }
}

struct FramePool(Direct3D11CaptureFramePool);

impl Drop for FramePool {
  fn drop(&mut self) {
    let _ = self.0.Close();
  }
}

struct CaptureSession(GraphicsCaptureSession);

impl Drop for CaptureSession {
  fn drop(&mut self) {
    let _ = self.0.Close();
  }
}

struct Frame(Option<Direct3D11CaptureFrame>);

impl Frame {
  fn get(&self) -> &Direct3D11CaptureFrame {
    self.0.as_ref().expect("open capture frame")
  }

  fn close(mut self) -> Result<()> {
    self.0.take().expect("open capture frame").Close()?;
    Ok(())
  }
}

impl Drop for Frame {
  fn drop(&mut self) {
    if let Some(frame) = self.0.take() {
      let _ = frame.Close();
    }
  }
}

fn next_frame(pool: &Direct3D11CaptureFramePool) -> windows::core::Result<Option<Frame>> {
  // The generated wrapper treats successful null as an error. WGC documents
  // S_OK + null as an empty pool; preserve that distinction from real failures.
  let mut raw = std::ptr::null_mut();
  unsafe {
    (pool.vtable().TryGetNextFrame)(pool.as_raw(), &mut raw).ok()?;
    Ok((!raw.is_null()).then(|| Frame(Some(Direct3D11CaptureFrame::from_raw(raw)))))
  }
}

fn dimensions(size: SizeInt32) -> Result<[usize; 2]> {
  Ok(pixels::output_size(
    [usize::try_from(size.Width)?, usize::try_from(size.Height)?],
    Rotation::Identity,
  )?)
}

enum FrameUpdate<'a> {
  Empty,
  Resized,
  Clipped {
    texture: [usize; 2],
    content: [usize; 2],
  },
  Ready {
    extent: [usize; 2],
    pixels: &'a [u8],
  },
}

fn poll_frame<'a>(
  pool: &Direct3D11CaptureFramePool,
  device: &IDirect3DDevice,
  size: &mut SizeInt32,
  reader: &'a mut TextureReader,
) -> Result<FrameUpdate<'a>> {
  let Some(mut frame) = next_frame(pool)? else {
    return Ok(FrameUpdate::Empty);
  };
  // Prefer the newest queued frame without an unbounded drain under animation.
  if let Some(newer) = next_frame(pool)? {
    frame.close()?;
    frame = newer;
  }
  let content = frame.get().ContentSize()?;
  let extent = dimensions(content)?;
  if content != *size {
    // The old texture may clip growing content or contain undefined pixels
    // after shrink. Return it before replacing the pool and emit neither case.
    frame.close()?;
    pool.Recreate(device, FORMAT, 2, content)?;
    *size = content;
    return Ok(FrameUpdate::Resized);
  }
  let surface = frame.get().Surface()?;
  let texture: ID3D11Texture2D = unsafe {
    surface
      .cast::<IDirect3DDxgiInterfaceAccess>()?
      .GetInterface()
  }?;
  let update = match reader.read_window(&texture, extent)? {
    WindowReadback::Complete(pixels) => FrameUpdate::Ready { extent, pixels },
    WindowReadback::Clipped(texture) => FrameUpdate::Clipped {
      texture,
      content: extent,
    },
  };
  drop(texture);
  drop(surface);
  frame.close()?;
  Ok(update)
}

/// Captures a selected window's compositor surface, independent of its occlusion.
pub struct WgcCapture {
  target: WindowTarget,
}

impl WgcCapture {
  pub fn open_desktop(id: &str) -> Result<Self> {
    Ok(Self {
      target: WindowTarget::open_desktop(id)?,
    })
  }
}

impl CaptureSource for WgcCapture {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let _physical = PhysicalPixels::enter()?;
    // Locals drop in reverse order: all WinRT resources are released before MTA exit.
    let _runtime = Runtime::enter()?;
    if !GraphicsCaptureSession::IsSupported()? {
      return Err("Windows Graphics Capture is unavailable in this desktop session".into());
    }
    self.target.validate()?;
    // WGC accepts a native D3D11 device independently of monitor duplication.
    // Default hardware selection also permits capture after the window moves.
    let mut reader = TextureReader::new(None)?;
    reader.enable_thread_protection()?;
    let device: IDirect3DDevice =
      unsafe { CreateDirect3D11DeviceFromDXGIDevice(&reader.device().cast::<IDXGIDevice>()?) }?
        .cast()?;
    let item = CaptureItem::new(&self.target)?;
    let mut size = item.item.Size()?;
    dimensions(size)?;
    let pool = FramePool(Direct3D11CaptureFramePool::CreateFreeThreaded(
      &device, FORMAT, 2, size,
    )?);
    let session = CaptureSession(pool.0.CreateCaptureSession(&item.item)?);
    // Keep the OS capture indicator and default cursor policy. Borderless capture
    // requires separate consent; neither policy is changed by this provider.
    session.0.StartCapture()?;
    let mut complete_deadline = Some(Instant::now() + FRAME_RECOVERY);
    let mut clipped_geometry = None;
    let mut delay = Duration::ZERO;
    while !lifetime.cancelled(delay) && !item.is_closed() {
      let update = self.target.validate().and_then(|()| {
        if complete_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
          let details = clipped_geometry
            .map(|(texture, content): ([usize; 2], [usize; 2])| {
              format!(
                ": texture {}x{}, content {}x{}",
                texture[0], texture[1], content[0], content[1]
              )
            })
            .unwrap_or_default();
          return Err(format!("Windows window capture produced no complete frame after startup or resize{details}; content may be protected or unavailable").into());
        }
        poll_frame(&pool.0, &device, &mut size, &mut reader)
      });
      delay = POLL_INTERVAL;
      match update {
        // Win32 and WGC can observe destruction before the Closed callback.
        Err(_) if item.is_closed() || self.target.is_gone().unwrap_or(false) => break,
        Err(error) => return Err(error),
        Ok(FrameUpdate::Empty) => {}
        Ok(FrameUpdate::Resized) => {
          complete_deadline = Some(Instant::now() + FRAME_RECOVERY);
          clipped_geometry = None;
        }
        Ok(FrameUpdate::Clipped { texture, content }) => {
          // Producer frames can overlap recreation. Await a sufficiently large
          // buffer rather than emitting a clipped frame or failing the session.
          complete_deadline.get_or_insert_with(|| Instant::now() + FRAME_RECOVERY);
          clipped_geometry = Some((texture, content));
        }
        Ok(FrameUpdate::Ready {
          extent: [width, height],
          pixels,
        }) => {
          if lifetime.cancelled(Duration::ZERO) || item.is_closed() {
            break;
          }
          write!(output, "P6\n{width} {height}\n255\n")?;
          output.write_all(pixels)?;
          output.flush()?;
          complete_deadline = None;
          clipped_geometry = None;
          delay = FRAME_INTERVAL;
        }
      }
    }
    Ok(())
  }
}
