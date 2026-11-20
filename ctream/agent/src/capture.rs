//! Metadata and selection for the bundled agent's capture providers.
//!
//! Custom agents can supply their own catalog and host their typed sources with
//! `runtime::capture_with_metadata`; no global registry is required.

use ctream_driver::Result;
use ctream_proto::{CaptureDriverInfo, CaptureFormat, CaptureScope, Target, X11_FRAME_SIZE};

pub struct Selection {
  pub id: &'static str,
  pub name: &'static str,
  pub scope: CaptureScope,
  pub format: CaptureFormat,
}

fn entries(target: Target) -> [Selection; 2] {
  let (display, display_name, window, window_name, format) = match target {
    Target::X11(_) => (
      "linux.x11.ffmpeg",
      "FFmpeg X11",
      "linux.x11.composite",
      "X11 Composite",
      CaptureFormat::FixedRgb {
        size: X11_FRAME_SIZE,
      },
    ),
    Target::Wayland(_) => (
      "linux.wayland.grim",
      "Grim",
      "linux.wayland.image_copy",
      "Wayland image copy",
      CaptureFormat::Ppm,
    ),
    Target::Windows(_) => (
      "windows.dxgi",
      "DXGI Desktop Duplication",
      "windows.wgc",
      "Windows Graphics Capture",
      CaptureFormat::Ppm,
    ),
  };
  [
    Selection {
      id: display,
      name: display_name,
      scope: CaptureScope::Display,
      format,
    },
    Selection {
      id: window,
      name: window_name,
      scope: CaptureScope::Window,
      format: CaptureFormat::Ppm,
    },
  ]
}

/// The platform host must validate its target platform before returning this
/// compile-supplied catalog. Runtime permissions are checked when capture opens.
pub fn list(target: Target) -> Vec<CaptureDriverInfo> {
  entries(target)
    .into_iter()
    .map(|entry| CaptureDriverInfo {
      id: entry.id.into(),
      name: entry.name.into(),
      scopes: vec![entry.scope],
    })
    .collect()
}

pub fn select(target: Target, requested: &str, window: bool) -> Result<Selection> {
  let scope = if window {
    CaptureScope::Window
  } else {
    CaptureScope::Display
  };
  entries(target)
    .into_iter()
    .find(|entry| entry.scope == scope && (requested == "auto" || requested == entry.id))
    .ok_or_else(|| {
      "capture driver is unknown or does not support the selected target and scope".into()
    })
}

#[cfg(test)]
mod tests {
  use super::*;

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
  }
}
