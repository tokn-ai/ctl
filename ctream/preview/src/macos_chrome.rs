use super::chrome::{BUTTON_STRIP_WIDTH, CONTROLS_TITLE, VIEWER_TITLE};
use block2::RcBlock;
use eframe::egui;
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
  NSApplication, NSEvent, NSEventMask, NSEventType, NSScreen, NSWindow, NSWindowCollectionBehavior,
  NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use std::{cell::Cell, ptr::NonNull, rc::Rc};

#[derive(Clone)]
pub(super) struct ViewerWindow {
  application: Retained<NSApplication>,
  window: Retained<NSWindow>,
}

pub(super) struct AttachedPanel {
  viewer: ViewerWindow,
  panel: Retained<NSWindow>,
  monitor: Retained<AnyObject>,
  drag: Rc<Cell<Option<Drag>>>,
}

#[derive(Clone, Copy)]
struct Drag {
  press: NSPoint,
  viewer_origin: NSPoint,
  panel_offset_y: f64,
}

impl Drag {
  fn viewer_at(self, pointer: NSPoint) -> NSPoint {
    NSPoint::new(
      self.viewer_origin.x + (pointer.x - self.press.x),
      self.viewer_origin.y + (pointer.y - self.press.y),
    )
  }

  fn keep_panel_in_work_area(self, mut viewer: NSPoint, height: f64, area: NSRect) -> NSPoint {
    let bottom = area.min().y;
    let top = area.max().y - height;
    if bottom <= top {
      let y = viewer.y + self.panel_offset_y;
      viewer.y += y.clamp(bottom, top) - y;
    }
    viewer
  }
}

fn own_window(application: &NSApplication, title: &str) -> Option<Retained<NSWindow>> {
  let title = NSString::from_str(title);
  application
    .windows()
    .to_vec()
    .into_iter()
    .find(|window| *window.title() == *title)
}

pub(super) fn viewer() -> Option<ViewerWindow> {
  let application = NSApplication::sharedApplication(MainThreadMarker::new()?);
  let window = own_window(&application, VIEWER_TITLE)?;
  Some(ViewerWindow {
    application,
    window,
  })
}

fn panel_at(video: NSRect, y: f64, height: f64) -> NSRect {
  // Use the viewer's full horizontal span in desktop coordinates. The window
  // server composites whichever portion of each window a display can show.
  NSRect::new(
    NSPoint::new(video.min().x, y),
    NSSize::new(video.size.width, height),
  )
}

fn panel_frame(video: NSRect, work_area: NSRect, height: f64) -> Option<NSRect> {
  if video.is_empty() || work_area.is_empty() || height <= 0.0 {
    return None;
  }
  [video.max().y, video.min().y - height]
    .into_iter()
    .find(|&y| y >= work_area.min().y && y + height <= work_area.max().y)
    .map(|y| panel_at(video, y, height))
}

impl ViewerWindow {
  pub(super) fn active(&self) -> bool {
    self.application.isActive() && self.window.isVisible() && !self.window.isMiniaturized()
  }

  fn panel_frame(&self, height: f32) -> Option<NSRect> {
    panel_frame(
      self.window.frame(),
      self.window.screen()?.visibleFrame(),
      f64::from(height),
    )
  }

  pub(super) fn panel_width(&self, height: f32) -> Option<f64> {
    Some(self.panel_frame(height)?.size.width)
  }

  pub(super) fn attach_controls(&self, height: f32, ctx: &egui::Context) -> Option<AttachedPanel> {
    let frame = self.panel_frame(height)?;
    let panel = own_window(&self.application, CONTROLS_TITLE)?;
    let drag = Rc::new(Cell::new(None));
    let monitor = monitor_drag(&self.window, &panel, &drag, ctx)?;
    panel.setFrame_display(frame, false);
    panel.setHasShadow(false);
    panel.setExcludedFromWindowsMenu(true);
    panel.setCollectionBehavior(
      NSWindowCollectionBehavior::Transient
        | NSWindowCollectionBehavior::IgnoresCycle
        | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    panel.setHidesOnDeactivate(true);
    // The viewer and controls are distinct windows owned by our application.
    unsafe {
      self
        .window
        .addChildWindow_ordered(&panel, NSWindowOrderingMode::Above);
    }
    panel.orderFront(None);
    Some(AttachedPanel {
      viewer: self.clone(),
      panel,
      monitor,
      drag,
    })
  }
}

fn drag_hit(point: NSPoint, size: NSSize, button_strip_width: f32) -> bool {
  NSPointInRect(
    point,
    NSRect::new(
      NSPoint::ZERO,
      NSSize::new(
        (size.width - f64::from(button_strip_width)).max(0.0),
        size.height,
      ),
    ),
  )
}

fn monitor_drag(
  viewer: &Retained<NSWindow>,
  panel: &Retained<NSWindow>,
  drag: &Rc<Cell<Option<Drag>>>,
  ctx: &egui::Context,
) -> Option<Retained<AnyObject>> {
  let mtm = MainThreadMarker::new()?;
  let viewer = viewer.clone();
  let panel = panel.clone();
  let drag = Rc::clone(drag);
  let ctx = ctx.clone();
  let handler = RcBlock::new(move |event: NonNull<NSEvent>| {
    // AppKit supplies a live event on the main thread for this call.
    let report = unsafe { event.as_ref() };
    let location = report.locationInWindow();
    let screen_point = || {
      report
        .window(mtm)
        .map_or(location, |window| window.convertPointToScreen(location))
    };
    match report.r#type() {
      NSEventType::LeftMouseDown => {
        drag.set(None);
        let panel_frame = panel.frame();
        if report.windowNumber() != panel.windowNumber()
          || !drag_hit(
            location,
            panel_frame.size,
            BUTTON_STRIP_WIDTH * ctx.zoom_factor(),
          )
        {
          return event.as_ptr();
        }
        let viewer_origin = viewer.frame().origin;
        drag.set(Some(Drag {
          press: screen_point(),
          viewer_origin,
          panel_offset_y: panel_frame.origin.y - viewer_origin.y,
        }));
      }
      NSEventType::LeftMouseDragged | NSEventType::LeftMouseUp => {
        let tracked = if report.r#type() == NSEventType::LeftMouseUp {
          drag.take()
        } else {
          drag.get()
        };
        let Some(tracked) = tracked else {
          return event.as_ptr();
        };
        // Queued mouse events can still carry coordinates relative to the
        // panel's previous origin. Sample the pointer in screen coordinates.
        let pointer = NSEvent::mouseLocation();
        let origin = tracked.viewer_at(pointer);
        let screen = NSScreen::screens(mtm)
          .to_vec()
          .into_iter()
          .find(|screen| NSPointInRect(pointer, screen.frame()))
          .or_else(|| viewer.screen());
        // Reserve room vertically for the grabbed strip. AppKit would
        // otherwise constrain the child independently at the menu bar or Dock.
        let height = panel.frame().size.height;
        let origin = screen.map_or(origin, |screen| {
          tracked.keep_panel_in_work_area(origin, height, screen.visibleFrame())
        });
        if origin != viewer.frame().origin {
          viewer.setFrameOrigin(origin);
        }
        let video = viewer.frame();
        let frame = panel_at(video, video.min().y + tracked.panel_offset_y, height);
        if frame != panel.frame() {
          panel.setFrame_display(frame, false);
        }
      }
      _ => return event.as_ptr(),
    }
    ctx.request_repaint_of(egui::ViewportId::ROOT);
    std::ptr::null_mut()
  });
  // Return the provided event unchanged, or null to consume a tracked drag.
  unsafe {
    NSEvent::addLocalMonitorForEventsMatchingMask_handler(
      NSEventMask::LeftMouseDown | NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp,
      &handler,
    )
  }
}

impl AttachedPanel {
  pub(super) fn dragging(&self) -> bool {
    self.drag.get().is_some()
  }

  pub(super) fn hovering(&self, reveal: f32) -> bool {
    let pointer = NSEvent::mouseLocation();
    let video = self.viewer.window.frame();
    let panel = self.panel.frame();
    // A panel below the viewer needs to stay open while the pointer crosses
    // the video from the top-edge reveal strip.
    let below = panel.max().y <= video.min().y;
    NSPointInRect(pointer, panel)
      || (NSPointInRect(pointer, video)
        && (below || pointer.y >= video.max().y - f64::from(reveal)))
  }

  pub(super) fn place(&self, height: f32) -> bool {
    let Some(frame) = self.viewer.panel_frame(height) else {
      return false;
    };
    if self.panel.frame() != frame {
      self.panel.setFrame_display(frame, false);
    }
    true
  }

  pub(super) fn restore_focus(&self) {
    if self.panel.isKeyWindow() && self.viewer.active() {
      self.viewer.window.makeKeyWindow();
    }
  }
}

impl Drop for AttachedPanel {
  fn drop(&mut self) {
    // Remove this panel's monitor before hiding the window and dropping the
    // retained objects captured by the handler.
    unsafe {
      NSEvent::removeMonitor(&self.monitor);
    }
    let panel_was_key = self.panel.isKeyWindow();
    self.viewer.window.removeChildWindow(&self.panel);
    self.panel.orderOut(None);
    if panel_was_key && self.viewer.active() {
      self.viewer.window.makeKeyWindow();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
  }

  #[test]
  fn title_panel_prefers_the_space_above_the_viewer() {
    let desktop = rect(-800.0, 24.0, 800.0, 580.0);
    let video = rect(-720.0, 180.0, 640.0, 360.0);
    assert_eq!(
      panel_frame(video, desktop, 34.0),
      Some(rect(-720.0, 540.0, 640.0, 34.0))
    );
  }

  #[test]
  fn title_panel_uses_visible_space_below_when_the_top_is_blocked() {
    let desktop = rect(0.0, 40.0, 800.0, 560.0);
    let video = rect(100.0, 200.0, 640.0, 400.0);
    assert_eq!(
      panel_frame(video, desktop, 34.0),
      Some(rect(100.0, 166.0, 640.0, 34.0))
    );
    assert_eq!(panel_frame(desktop, desktop, 34.0), None);
  }

  #[test]
  fn title_panel_keeps_the_viewers_full_width_at_either_edge() {
    let desktop = rect(0.0, 40.0, 1440.0, 820.0);
    for (video, expected) in [
      (
        rect(-180.0, 100.0, 620.0, 360.0),
        rect(-180.0, 460.0, 620.0, 34.0),
      ),
      (
        rect(1040.0, 100.0, 620.0, 360.0),
        rect(1040.0, 460.0, 620.0, 34.0),
      ),
    ] {
      assert_eq!(panel_frame(video, desktop, 34.0), Some(expected));
    }
  }

  #[test]
  fn title_panel_spans_displays_regardless_of_which_owns_the_viewer() {
    let left = rect(-800.0, 40.0, 800.0, 820.0);
    let right = rect(0.0, 40.0, 1440.0, 820.0);
    let video = rect(-540.0, 100.0, 1740.0, 360.0);
    for desktop in [left, right] {
      assert_eq!(
        panel_frame(video, desktop, 34.0),
        Some(rect(-540.0, 460.0, 1740.0, 34.0))
      );
    }
  }

  #[test]
  fn viewer_moves_from_the_original_screen_anchor() {
    let drag = Drag {
      press: NSPoint::new(-402.0, 420.0),
      viewer_origin: NSPoint::new(-550.0, 205.0),
      panel_offset_y: 360.0,
    };
    assert_eq!(drag.viewer_at(drag.press), drag.viewer_origin);
    let moved = drag.viewer_at(NSPoint::new(101.0, 380.0));
    assert_eq!(moved, NSPoint::new(-47.0, 165.0));
    assert_eq!(moved.y + drag.panel_offset_y, 525.0);
  }

  #[test]
  fn native_drag_area_excludes_the_window_buttons() {
    let size = NSSize::new(400.0, 34.0);
    assert!(drag_hit(NSPoint::new(279.0, 17.0), size, 120.0));
    assert!(!drag_hit(NSPoint::new(280.0, 17.0), size, 120.0));
    assert!(!drag_hit(NSPoint::new(300.0, 17.0), size, 120.0));
  }

  #[test]
  fn dragging_keeps_the_grabbed_edge_inside_the_work_area() {
    let area = rect(0.0, 40.0, 800.0, 560.0);
    let above = Drag {
      press: NSPoint::ZERO,
      viewer_origin: NSPoint::new(100.0, 100.0),
      panel_offset_y: 360.0,
    };
    let desired = NSPoint::new(200.0, 300.0);
    let moved = above.keep_panel_in_work_area(desired, 34.0, area);
    assert_eq!(moved, NSPoint::new(200.0, 206.0));
    assert_eq!(moved.y + above.panel_offset_y, 566.0);

    let below = Drag {
      panel_offset_y: -34.0,
      ..above
    };
    let moved = below.keep_panel_in_work_area(NSPoint::new(200.0, 0.0), 34.0, area);
    assert_eq!(moved, NSPoint::new(200.0, 74.0));
    assert_eq!(moved.y + below.panel_offset_y, 40.0);
  }
}
