//! Metadata and selection for the bundled agent's capture providers.
//!
//! Custom agents can supply their own catalog and host their typed sources with
//! `runtime::capture_with_metadata` or `runtime::capture_video_with_metadata`.

#[cfg(any(target_os = "linux", windows))]
pub(crate) mod platform;
pub mod pool;

use crate::runtime::{capture_video_with_metadata, capture_with_metadata};
use ctream_driver::{CaptureDisplay, CaptureOptions, CaptureSource, CaptureWindow, Result};
use ctream_proto::{
  CaptureDriverDescriptor, CaptureDriverInfo, CaptureFormat, CaptureScope, CaptureSystem, Target,
  VideoRequest, X11_FRAME_SIZE,
};

#[derive(Clone, Copy)]
pub struct Selection {
  pub id: &'static str,
  pub name: &'static str,
  pub scope: CaptureScope,
  pub format: CaptureFormat,
}

impl Selection {
  pub fn capture_display(
    self,
    session: impl CaptureDisplay,
    video: Option<VideoRequest>,
  ) -> Result<()> {
    let source = match options(video.as_ref())? {
      Some(options) => session.capture_display_with_options(options)?,
      None => session.capture_display()?,
    };
    self.stream(source, video)
  }

  pub fn capture_window(
    self,
    session: impl CaptureWindow,
    id: &str,
    video: Option<VideoRequest>,
  ) -> Result<()> {
    let source = match options(video.as_ref())? {
      Some(options) => session.capture_window_with_options(id, options)?,
      None => session.capture_window(id)?,
    };
    self.stream(source, video)
  }

  pub fn stream(self, source: impl CaptureSource, video: Option<VideoRequest>) -> Result<()> {
    match video {
      Some(request) => capture_video_with_metadata(source, self.id, self.format, request),
      None => capture_with_metadata(source, self.id, self.format),
    }
  }
}

pub const MACOS_CAPTURE_DRIVER: &str = "macos.screencapturekit";

/// Convert stream settings before opening a source. Raw requests keep their
/// existing provider defaults.
pub fn options(video: Option<&VideoRequest>) -> Result<Option<CaptureOptions>> {
  video
    .map(|request| CaptureOptions::new(request.options.max_fps))
    .transpose()
}

fn entries(system: CaptureSystem) -> &'static [Selection] {
  match system {
    CaptureSystem::X11 => &[
      Selection {
        id: "linux.x11.ffmpeg",
        name: "FFmpeg X11",
        scope: CaptureScope::Display,
        format: CaptureFormat::FixedRgb {
          size: X11_FRAME_SIZE,
        },
      },
      Selection {
        id: "linux.x11.composite",
        name: "X11 Composite",
        scope: CaptureScope::Window,
        format: CaptureFormat::Ppm,
      },
    ],
    CaptureSystem::Wayland => &[
      Selection {
        id: "linux.wayland.grim",
        name: "Grim",
        scope: CaptureScope::Display,
        format: CaptureFormat::Ppm,
      },
      Selection {
        id: "linux.wayland.image_copy",
        name: "Wayland image copy",
        scope: CaptureScope::Window,
        format: CaptureFormat::Ppm,
      },
    ],
    CaptureSystem::Windows => &[
      Selection {
        id: "windows.dxgi",
        name: "DXGI Desktop Duplication",
        scope: CaptureScope::Display,
        format: CaptureFormat::Ppm,
      },
      Selection {
        id: "windows.wgc",
        name: "Windows Graphics Capture",
        scope: CaptureScope::Window,
        format: CaptureFormat::Ppm,
      },
    ],
    CaptureSystem::Macos => &[Selection {
      id: MACOS_CAPTURE_DRIVER,
      name: "ScreenCaptureKit",
      scope: CaptureScope::Window,
      format: CaptureFormat::Ppm,
    }],
  }
}

/// The platform host must validate its target platform before returning this
/// compile-supplied catalog. Runtime permissions are checked when capture opens.
pub fn list(target: Target) -> Vec<CaptureDriverInfo> {
  descriptors(target.system())
    .filter(|descriptor| {
      !target.window_only() || descriptor.info.scopes.contains(&CaptureScope::Window)
    })
    .map(|descriptor| descriptor.info)
    .collect()
}

fn descriptors(system: CaptureSystem) -> impl Iterator<Item = CaptureDriverDescriptor> {
  entries(system)
    .iter()
    .map(move |entry| CaptureDriverDescriptor {
      info: CaptureDriverInfo {
        id: entry.id.into(),
        name: entry.name.into(),
        scopes: vec![entry.scope],
      },
      systems: vec![system],
    })
}

/// Every provider compiled into the stock platform agent, even with no running
/// displays. This is capability metadata; opening a source checks runtime access.
pub fn catalog() -> Vec<CaptureDriverDescriptor> {
  let systems: &[CaptureSystem] = if cfg!(target_os = "linux") {
    &[CaptureSystem::X11, CaptureSystem::Wayland]
  } else if cfg!(windows) {
    &[CaptureSystem::Windows]
  } else if cfg!(target_os = "macos") {
    &[CaptureSystem::Macos]
  } else {
    &[]
  };
  systems.iter().copied().flat_map(descriptors).collect()
}

pub fn select(target: Target, requested: &str, window: bool) -> Result<Selection> {
  if target == Target::WindowsDesktop && !window {
    return Err("Windows desktop target requires a window ID".into());
  }
  if target == Target::MacosDesktop && !window {
    return Err("macOS desktop target requires a window ID".into());
  }
  let scope = if window {
    CaptureScope::Window
  } else {
    CaptureScope::Display
  };
  entries(target.system())
    .iter()
    .copied()
    .find(|entry| entry.scope == scope && (requested == "auto" || requested == entry.id))
    .ok_or_else(|| {
      "capture driver is unknown or does not support the selected target and scope".into()
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn catalog_reports_compiled_systems_without_resolving_display_instances() {
    let catalog = catalog();
    let expected = if cfg!(target_os = "linux") {
      vec![
        "linux.x11.ffmpeg",
        "linux.x11.composite",
        "linux.wayland.grim",
        "linux.wayland.image_copy",
      ]
    } else if cfg!(windows) {
      vec!["windows.dxgi", "windows.wgc"]
    } else if cfg!(target_os = "macos") {
      vec![MACOS_CAPTURE_DRIVER]
    } else {
      vec![]
    };
    assert_eq!(
      catalog
        .iter()
        .map(|driver| driver.info.id.as_str())
        .collect::<Vec<_>>(),
      expected
    );
    for system in [
      CaptureSystem::X11,
      CaptureSystem::Wayland,
      CaptureSystem::Windows,
    ] {
      let descriptors: Vec<_> = descriptors(system).collect();
      assert!(descriptors.iter().all(|driver| driver.systems == [system]));
      assert_eq!(descriptors[0].info.scopes, [CaptureScope::Display]);
      assert_eq!(descriptors[1].info.scopes, [CaptureScope::Window]);
    }
    let macos = descriptors(CaptureSystem::Macos).collect::<Vec<_>>();
    assert_eq!(macos.len(), 1);
    assert_eq!(macos[0].systems, [CaptureSystem::Macos]);
    assert_eq!(macos[0].info.scopes, [CaptureScope::Window]);
  }

  #[test]
  fn defaults_and_explicit_selection_preserve_scope_without_fallback() {
    for target in [Target::X11(7), Target::Wayland(1), Target::Windows(2)] {
      let catalog = list(target);
      for (window, index) in [(false, 0), (true, 1)] {
        let selected = select(target, "auto", window).unwrap();
        assert_eq!(selected.id, catalog[index].id);
        assert_eq!(select(target, selected.id, window).unwrap().id, selected.id);
        assert!(select(target, &catalog[1 - index].id, window).is_err());
        assert!(select(target, "user.unknown", window).is_err());
      }
    }
    assert!(select(Target::X11(7), "windows.dxgi", false).is_err());
    let desktop = Target::WindowsDesktop;
    assert!(select(desktop, "auto", false).is_err());
    assert_eq!(select(desktop, "auto", true).unwrap().id, "windows.wgc");
    let catalog = list(desktop);
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].id, "windows.wgc");
    let macos = Target::MacosDesktop;
    assert_eq!(list(macos).len(), 1);
    assert_eq!(
      select(macos, "auto", true).unwrap().id,
      MACOS_CAPTURE_DRIVER
    );
    assert!(select(macos, "auto", false).is_err());
  }
}
