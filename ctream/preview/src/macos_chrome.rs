use super::chrome::{CONTROLS_TITLE, VIEWER_TITLE};
use block2::RcBlock;
use eframe::egui;
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
  NSApplication, NSEvent, NSEventMask, NSEventType, NSScreen, NSWindow, NSWindowCollectionBehavior,
  NSWindowOcclusionState, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use std::{cell::Cell, ptr::NonNull, rc::Rc};

#[path = "macos_chrome/placement.rs"]
mod placement;

use placement::{keep_panel_visible, panel_frame, reveal_at};

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
  drag_limit: Rc<Cell<f64>>,
}

#[derive(Clone, Copy)]
struct Drag {
  press: NSPoint,
  viewer_origin: NSPoint,
}

impl Drag {
  fn viewer_at(self, pointer: NSPoint) -> NSPoint {
    NSPoint::new(
      self.viewer_origin.x + (pointer.x - self.press.x),
      self.viewer_origin.y + (pointer.y - self.press.y),
    )
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

pub(super) fn redraw_visible_viewer() {
  let Some(viewer) = viewer() else { return };
  let window = &viewer.window;
  if window.isVisible()
    && !window.isMiniaturized()
    && window
      .occlusionState()
      .contains(NSWindowOcclusionState::Visible)
    && let Some(content) = window.contentView()
  {
    content.setNeedsDisplay(true);
  }
}

fn work_areas(main: MainThreadMarker) -> Vec<NSRect> {
  NSScreen::screens(main)
    .to_vec()
    .iter()
    .map(|screen| screen.visibleFrame())
    .collect()
}

fn place_panel(panel: &NSWindow, frame: NSRect) {
  if panel.frame() != frame {
    panel.setFrame_display(frame, false);
  }
}

fn panel_overlap(panel: NSRect, video: NSRect, content: egui::Rect) -> egui::Rect {
  if video.is_empty() || !content.is_positive() {
    return egui::Rect::NOTHING;
  }
  let left = panel.min().x.max(video.min().x);
  let right = panel.max().x.min(video.max().x);
  let bottom = panel.min().y.max(video.min().y);
  let top = panel.max().y.min(video.max().y);
  if left >= right || bottom >= top {
    return egui::Rect::NOTHING;
  }
  let scale_x = f64::from(content.width()) / video.size.width;
  let scale_y = f64::from(content.height()) / video.size.height;
  let to_point = |x, y| {
    content.min
      + egui::vec2(
        ((x - video.min().x) * scale_x) as f32,
        ((video.max().y - y) * scale_y) as f32,
      )
  };
  egui::Rect::from_min_max(to_point(left, top), to_point(right, bottom))
}

impl ViewerWindow {
  pub(super) fn active(&self) -> bool {
    self.application.isActive() && self.window.isVisible() && !self.window.isMiniaturized()
  }

  fn panel_frame(&self, height: f32, min_width: f32) -> Option<NSRect> {
    panel_frame(
      self.window.frame(),
      &work_areas(MainThreadMarker::new()?),
      f64::from(height),
      f64::from(min_width),
    )
  }

  pub(super) fn panel_width(&self, height: f32, min_width: f32) -> Option<f64> {
    Some(self.panel_frame(height, min_width)?.size.width)
  }

  pub(super) fn reveal_hovering(&self, height: f32, min_width: f32, reveal: f32) -> bool {
    let Some(main) = MainThreadMarker::new() else {
      return false;
    };
    let video = self.window.frame();
    let areas = work_areas(main);
    let Some(panel) = panel_frame(video, &areas, f64::from(height), f64::from(min_width)) else {
      return false;
    };
    reveal_at(
      video,
      &areas,
      panel,
      NSEvent::mouseLocation(),
      f64::from(reveal),
      false,
    )
  }

  pub(super) fn attach_controls(
    &self,
    height: f32,
    min_width: f32,
    ctx: &egui::Context,
  ) -> Option<AttachedPanel> {
    let frame = self.panel_frame(height, min_width)?;
    let panel = own_window(&self.application, CONTROLS_TITLE)?;
    let drag = Rc::new(Cell::new(None));
    let drag_limit = Rc::new(Cell::new(0.0));
    let monitor = monitor_drag(&self.window, &panel, &drag, min_width, &drag_limit, ctx)?;
    place_panel(&panel, frame);
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
      drag_limit,
    })
  }
}

fn drag_hit(point: NSPoint, size: NSSize, right_edge: f64) -> bool {
  NSPointInRect(
    point,
    NSRect::new(
      NSPoint::ZERO,
      NSSize::new(right_edge.min(size.width).max(0.0), size.height),
    ),
  )
}

fn monitor_drag(
  viewer: &Retained<NSWindow>,
  panel: &Retained<NSWindow>,
  drag: &Rc<Cell<Option<Drag>>>,
  min_width: f32,
  drag_limit: &Rc<Cell<f64>>,
  ctx: &egui::Context,
) -> Option<Retained<AnyObject>> {
  let mtm = MainThreadMarker::new()?;
  let viewer = viewer.clone();
  let panel = panel.clone();
  let drag = Rc::clone(drag);
  let drag_limit = Rc::clone(drag_limit);
  let viewport = ctx.viewport_id();
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
          || !drag_hit(location, panel_frame.size, drag_limit.get())
        {
          return event.as_ptr();
        }
        let viewer_origin = viewer.frame().origin;
        drag.set(Some(Drag {
          press: screen_point(),
          viewer_origin,
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
        let origin = tracked.viewer_at(NSEvent::mouseLocation());
        let previous = panel.frame();
        if origin != viewer.frame().origin {
          viewer.setFrameOrigin(origin);
        }
        // AppKit moves a child along with its parent. If the viewer has no
        // attachment point, hold the controls in their last reachable place
        // until the owned drag ends or another place becomes available.
        let areas = work_areas(mtm);
        let min_width = f64::from(min_width);
        let frame = panel_frame(viewer.frame(), &areas, previous.size.height, min_width)
          .or_else(|| keep_panel_visible(previous, &areas, min_width))
          .unwrap_or(previous);
        place_panel(&panel, frame);
      }
      _ => return event.as_ptr(),
    }
    ctx.request_repaint_of(viewport);
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

  pub(super) fn width(&self) -> f64 {
    self.panel.frame().size.width
  }

  /// The latest painted title divider in native window points.
  pub(super) fn set_drag_limit(&self, right_edge: f64) {
    self.drag_limit.set(right_edge);
  }

  pub(super) fn hovering(&self, reveal: f32) -> bool {
    let Some(main) = MainThreadMarker::new() else {
      return false;
    };
    let pointer = NSEvent::mouseLocation();
    let panel = self.panel.frame();
    let areas = work_areas(main);
    (NSPointInRect(pointer, panel) && areas.iter().any(|&area| NSPointInRect(pointer, area)))
      || reveal_at(
        self.viewer.window.frame(),
        &areas,
        panel,
        pointer,
        f64::from(reveal),
        true,
      )
  }

  pub(super) fn place(&self, height: f32, min_width: f32) -> bool {
    let Some(frame) = self.viewer.panel_frame(height, min_width) else {
      return false;
    };
    place_panel(&self.panel, frame);
    true
  }

  pub(super) fn overlay_rect(&self, content: egui::Rect) -> egui::Rect {
    let video = self
      .viewer
      .window
      .contentRectForFrameRect(self.viewer.window.frame());
    panel_overlap(self.panel.frame(), video, content)
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
  fn viewer_moves_from_the_original_screen_anchor() {
    let drag = Drag {
      press: NSPoint::new(-402.0, 420.0),
      viewer_origin: NSPoint::new(-550.0, 205.0),
    };
    assert_eq!(drag.viewer_at(drag.press), drag.viewer_origin);
    let moved = drag.viewer_at(NSPoint::new(101.0, 380.0));
    assert_eq!(moved, NSPoint::new(-47.0, 165.0));
  }

  #[test]
  fn dragging_to_the_top_moves_the_viewer_and_overlaps_only_the_panel() {
    let drag = Drag {
      press: NSPoint::new(200.0, 550.0),
      viewer_origin: NSPoint::new(100.0, 180.0),
    };
    let origin = drag.viewer_at(NSPoint::new(220.0, 610.0));
    assert_eq!(origin, NSPoint::new(120.0, 240.0));
    let video = NSRect::new(origin, NSSize::new(640.0, 360.0));
    let area = rect(0.0, 40.0, 800.0, 560.0);
    assert_eq!(
      panel_frame(video, &[area], 34.0, 200.0),
      Some(rect(120.0, 566.0, 640.0, 34.0))
    );
  }

  #[test]
  fn native_drag_area_excludes_the_window_buttons() {
    let size = NSSize::new(400.0, 34.0);
    for width in [120.0, 172.0] {
      let buttons_left = size.width - width;
      let hits_title = |x| drag_hit(NSPoint::new(x, 17.0), size, buttons_left);
      assert!(hits_title(buttons_left - 1.0));
      assert!(!hits_title(buttons_left));
      assert!(!hits_title(buttons_left + 20.0));
    }
  }

  #[test]
  fn native_drag_respects_the_rendered_divider_while_the_panel_widens() {
    let size = NSSize::new(560.0, 34.0);
    let stale = 400.0 - 172.0;
    let current = size.width - 172.0;
    let former_button = NSPoint::new(285.0, 17.0);
    assert!(!drag_hit(former_button, size, 0.0));
    assert!(!drag_hit(former_button, size, stale));
    assert!(drag_hit(former_button, size, current));
    assert!(!drag_hit(NSPoint::new(580.0, 17.0), size, 600.0));
  }

  #[test]
  fn overlap_excludes_only_the_covered_video_pixels_in_viewer_coordinates() {
    let video = rect(-100.0, 140.0, 400.0, 240.0);
    let content = egui::Rect::from_min_size(egui::pos2(2.0, 3.0), egui::vec2(200.0, 120.0));
    let panel = rect(-80.0, 350.0, 220.0, 34.0);
    assert_eq!(
      panel_overlap(panel, video, content),
      egui::Rect::from_min_max(egui::pos2(12.0, 3.0), egui::pos2(122.0, 18.0))
    );
    let above = rect(-80.0, 380.0, 220.0, 34.0);
    assert_eq!(panel_overlap(above, video, content), egui::Rect::NOTHING);
  }
}
