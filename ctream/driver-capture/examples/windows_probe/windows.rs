//! Read-only desktop-access diagnostics. No elevation, input, or pixel-file output.

use ::windows::{
  Win32::{
    Foundation::{E_FAIL, HANDLE, HMODULE},
    Graphics::{
      Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
      Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
      },
      Dxgi::{
        Common::DXGI_FORMAT_B8G8R8A8_UNORM, CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1,
        IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication,
      },
    },
    System::{
      RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
      StationsAndDesktops::{
        GetProcessWindowStation, GetThreadDesktop, GetUserObjectInformationW, UOI_FLAGS, UOI_NAME,
        USEROBJECTFLAGS,
      },
      Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
    UI::HiDpi::{
      AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
      GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    },
  },
  core::{Error, Interface, Result},
};
use serde_json::{Value, json};
use std::{mem::size_of, time::Instant};

const FRAME_TIMEOUT_MS: u32 = 1500;
const WSF_VISIBLE: u32 = 1;

fn failure(stage: &str, error: &Error) -> Value {
  json!({"status": "error", "stage": stage, "hresult": format!("0x{:08X}", error.code().0 as u32)})
}

fn dpi_awareness() -> Value {
  // GetDesc virtualizes desktop geometry for DPI-unaware callers. Set this before DXGI or UI APIs.
  let configured =
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
  let thread_per_monitor_v2 = unsafe {
    AreDpiAwarenessContextsEqual(
      GetThreadDpiAwarenessContext(),
      DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    )
  }
  .as_bool();
  json!({
    "requested": "per_monitor_v2",
    "process_default": configured.map_or_else(
      |error| failure("set_process_dpi_awareness", &error),
      |_| json!({"status": "set"}),
    ),
    "thread_per_monitor_v2": thread_per_monitor_v2,
    "desktop_coordinate_space": if thread_per_monitor_v2 { "physical_pixels" } else { "unverified" },
  })
}

fn utf16(value: &[u16]) -> String {
  let end = value
    .iter()
    .position(|unit| *unit == 0)
    .unwrap_or(value.len());
  String::from_utf16_lossy(&value[..end])
}

fn object_name(handle: HANDLE) -> Result<String> {
  let mut needed = 0;
  // Query byte size first; the initial insufficient-buffer error is expected.
  let result = unsafe { GetUserObjectInformationW(handle, UOI_NAME, None, 0, Some(&mut needed)) };
  if needed == 0 || needed > 65536 {
    return Err(result.err().unwrap_or_else(|| Error::from_hresult(E_FAIL)));
  }
  let mut buffer = vec![0_u16; (needed as usize).div_ceil(2)];
  unsafe {
    GetUserObjectInformationW(
      handle,
      UOI_NAME,
      Some(buffer.as_mut_ptr().cast()),
      needed,
      Some(&mut needed),
    )?;
  }
  Ok(utf16(&buffer))
}

fn context() -> Value {
  let mut session = 0;
  let process_session = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
  let active_console = unsafe { WTSGetActiveConsoleSessionId() };
  let station = unsafe { GetProcessWindowStation() }.map(|handle| {
    let mut flags = USEROBJECTFLAGS::default();
    let visibility = unsafe {
      GetUserObjectInformationW(
        HANDLE(handle.0), UOI_FLAGS, Some((&mut flags as *mut USEROBJECTFLAGS).cast()),
        size_of::<USEROBJECTFLAGS>() as u32, None,
      )
    };
    json!({
      "name": object_name(HANDLE(handle.0)).map_or_else(|e| failure("window_station_name", &e), |name| json!(name)),
      "visible": visibility.map_or_else(|e| failure("window_station_flags", &e), |_| json!(flags.dwFlags & WSF_VISIBLE != 0)),
    })
  });
  let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) }
    .and_then(|handle| object_name(HANDLE(handle.0)));
  json!({
    "process_session_id": process_session.as_ref().map_or_else(|e| failure("process_session", e), |_| json!(session)),
    "active_console_session_id": (active_console != u32::MAX).then_some(active_console),
    "same_session_as_console": process_session.is_ok().then_some(session == active_console && active_console != u32::MAX),
    "window_station": station.unwrap_or_else(|e| failure("window_station", &e)),
    "thread_desktop": desktop.map_or_else(|e| failure("thread_desktop", &e), |name| json!(name)),
  })
}

fn device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
  let mut device = None;
  let mut context = None;
  unsafe {
    D3D11CreateDevice(
      adapter,
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
  match (device, context) {
    (Some(device), Some(context)) => Ok((device, context)),
    _ => Err(Error::from_hresult(E_FAIL)),
  }
}

struct AcquiredFrame<'a>(Option<&'a IDXGIOutputDuplication>);

impl AcquiredFrame<'_> {
  fn release(mut self) -> Result<()> {
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

fn readback(
  device: &ID3D11Device,
  context: &ID3D11DeviceContext,
  texture: &ID3D11Texture2D,
) -> Value {
  let mut desc = D3D11_TEXTURE2D_DESC::default();
  unsafe { texture.GetDesc(&mut desc) };
  if desc.Width == 0
    || desc.Height == 0
    || desc.Width > 16384
    || desc.Height > 16384
    || u64::from(desc.Width) * u64::from(desc.Height) > 32 * 1024 * 1024
    || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
  {
    return json!({"status": "unsupported_texture", "width": desc.Width, "height": desc.Height, "format": desc.Format.0});
  }
  let staging_desc = D3D11_TEXTURE2D_DESC {
    Usage: D3D11_USAGE_STAGING,
    BindFlags: 0,
    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
    MiscFlags: 0,
    ..desc
  };
  let mut staging = None;
  if let Err(error) = unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) } {
    return failure("create_staging_texture", &error);
  }
  let Some(staging) = staging else {
    return failure("create_staging_texture", &Error::from_hresult(E_FAIL));
  };
  unsafe { context.CopyResource(&staging, texture) };
  let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
  if let Err(error) = unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) } {
    return failure("map_staging_texture", &error);
  }
  let _mapping = MappedTexture(context, &staging);
  let row_bytes = desc.Width as usize * 4;
  if mapped.pData.is_null() || (mapped.RowPitch as usize) < row_bytes {
    return json!({"status": "invalid_mapped_layout"});
  }
  let mut checksum = 0xcbf29ce484222325_u64;
  let mut nonblack_pixels = 0_u64;
  for row in 0..desc.Height as usize {
    // D3D guarantees each mapped row contains Width pixels, with RowPitch stride.
    let bytes = unsafe {
      std::slice::from_raw_parts(
        mapped
          .pData
          .cast::<u8>()
          .add(row * mapped.RowPitch as usize),
        row_bytes,
      )
    };
    nonblack_pixels += bytes
      .as_chunks::<4>()
      .0
      .iter()
      .filter(|pixel| pixel[..3] != [0, 0, 0])
      .count() as u64;
    for byte in bytes {
      checksum = (checksum ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
  }
  json!({"status": "read", "width": desc.Width, "height": desc.Height, "format": desc.Format.0,
    "pixel_bytes": u64::from(desc.Width) * u64::from(desc.Height) * 4,
    "nonblack_pixels": nonblack_pixels, "checksum_fnv1a64": format!("{checksum:016x}")})
}

fn capture(output: &IDXGIOutput, device: &ID3D11Device, context: &ID3D11DeviceContext) -> Value {
  let duplication = output
    .cast::<IDXGIOutput1>()
    .and_then(|output| unsafe { output.DuplicateOutput(device) });
  let duplication = match duplication {
    Ok(duplication) => duplication,
    Err(error) => return failure("duplicate_output", &error),
  };
  let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
  let mut resource = None;
  let started = Instant::now();
  if let Err(error) =
    unsafe { duplication.AcquireNextFrame(FRAME_TIMEOUT_MS, &mut info, &mut resource) }
  {
    let mut report = failure("acquire_next_frame", &error);
    if error.code() == DXGI_ERROR_WAIT_TIMEOUT {
      report["status"] = json!("timeout");
    }
    report["wait_ms"] = json!(started.elapsed().as_millis());
    return report;
  }
  let wait_ms = started.elapsed().as_millis();
  let frame = AcquiredFrame(Some(&duplication));
  let readback = match resource {
    Some(resource) => resource.cast::<ID3D11Texture2D>().map_or_else(
      |error| failure("frame_texture", &error),
      |texture| readback(device, context, &texture),
    ),
    None => failure("frame_resource", &Error::from_hresult(E_FAIL)),
  };
  let release = frame.release().map_or_else(
    |e| failure("release_frame", &e),
    |_| json!({"status": "released"}),
  );
  json!({"status": "frame_acquired", "wait_ms": wait_ms,
    "accumulated_frames": info.AccumulatedFrames, "readback": readback, "release": release})
}

fn adapters(factory: &IDXGIFactory1) -> Vec<Value> {
  let mut adapters = Vec::new();
  for adapter_index in 0..32 {
    let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
      Ok(adapter) => adapter,
      Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => return adapters,
      Err(error) => {
        adapters.push(failure("enumerate_adapters", &error));
        return adapters;
      }
    };
    let description = unsafe { adapter.GetDesc1() }.map_or_else(
      |error| failure("adapter_description", &error),
      |desc| json!({"name": utf16(&desc.Description), "software": desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0}),
    );
    let device = device(&adapter);
    let mut outputs = Vec::new();
    for output_index in 0..32 {
      let output = match unsafe { adapter.EnumOutputs(output_index) } {
        Ok(output) => output,
        Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
        Err(error) => {
          outputs.push(failure("enumerate_outputs", &error));
          break;
        }
      };
      match unsafe { output.GetDesc() } {
        Ok(desc) => {
          let capture = if !desc.AttachedToDesktop.as_bool() {
            json!({"status": "not_attached"})
          } else {
            device.as_ref().map_or_else(
              |error| failure("create_d3d11_device", error),
              |(device, context)| capture(&output, device, context),
            )
          };
          outputs.push(json!({"index": output_index, "attached": desc.AttachedToDesktop.as_bool(),
            "desktop_rect": [desc.DesktopCoordinates.left, desc.DesktopCoordinates.top, desc.DesktopCoordinates.right, desc.DesktopCoordinates.bottom],
            "rotation": desc.Rotation.0, "capture": capture}));
        }
        Err(error) => outputs.push(failure("output_description", &error)),
      }
    }
    let device_status = device.as_ref().map_or_else(
      |error| failure("create_d3d11_device", error),
      |_| json!({"status": "created"}),
    );
    adapters.push(json!({"index": adapter_index, "description": description, "device": device_status, "outputs": outputs}));
  }
  adapters.push(json!({"status": "enumeration_limit", "limit": 32}));
  adapters
}

pub fn probe() -> Value {
  let dpi_awareness = dpi_awareness();
  let factory = unsafe { CreateDXGIFactory1::<IDXGIFactory1>() };
  json!({"schema_version": 1, "dpi_awareness": dpi_awareness, "context": context(), "frame_timeout_ms": FRAME_TIMEOUT_MS,
    "adapters": factory.map_or_else(|error| failure("create_dxgi_factory", &error), |factory| json!(adapters(&factory)))})
}
