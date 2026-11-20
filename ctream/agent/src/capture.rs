//! Metadata and selection for the bundled agent's capture providers.
//!
//! Custom agents can supply their own catalog and host their typed sources with
//! `runtime::capture_with_metadata`; no global registry is required.

use ctream_driver::Result;
use ctream_proto::{
  CaptureDriverDescriptor, CaptureDriverInfo, CaptureFormat, CaptureScope, CaptureSystem, Target,
  X11_FRAME_SIZE,
};

pub struct Selection {
  pub id: &'static str,
  pub name: &'static str,
  pub scope: CaptureScope,
  pub format: CaptureFormat,
}

fn entries(system: CaptureSystem) -> [Selection; 2] {
  let (display, display_name, window, window_name, format) = match system {
    CaptureSystem::X11 => (
      "linux.x11.ffmpeg",
      "FFmpeg X11",
      "linux.x11.composite",
      "X11 Composite",
      CaptureFormat::FixedRgb {
        size: X11_FRAME_SIZE,
      },
    ),
    CaptureSystem::Wayland => (
      "linux.wayland.grim",
      "Grim",
      "linux.wayland.image_copy",
      "Wayland image copy",
      CaptureFormat::Ppm,
    ),
    CaptureSystem::Windows => (
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
  descriptors(target.system())
    .into_iter()
    .map(|descriptor| descriptor.info)
    .collect()
}

fn descriptors(system: CaptureSystem) -> [CaptureDriverDescriptor; 2] {
  entries(system).map(|entry| CaptureDriverDescriptor {
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
  } else {
    &[]
  };
  systems.iter().copied().flat_map(descriptors).collect()
}

pub fn select(target: Target, requested: &str, window: bool) -> Result<Selection> {
  let scope = if window {
    CaptureScope::Window
  } else {
    CaptureScope::Display
  };
  entries(target.system())
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
      let descriptors = descriptors(system);
      assert!(descriptors.iter().all(|driver| driver.systems == [system]));
      assert_eq!(descriptors[0].info.scopes, [CaptureScope::Display]);
      assert_eq!(descriptors[1].info.scopes, [CaptureScope::Window]);
    }
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
  }
}
