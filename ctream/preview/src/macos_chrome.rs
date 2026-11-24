use super::chrome::{CONTROLS_TITLE, VIEWER_TITLE};
use block2::RcBlock;
use eframe::egui;
use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
  NSApplication, NSEvent, NSEventMask, NSEventType, NSScreen, NSWindow, NSWindowCollectionBehavior,
  NSWindowOcclusionState, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSPointInRect, NSRect, NSSize, NSString};
use std::{
  cell::Cell,
  ptr::NonNull,
  rc::Rc,
  time::{Duration, Instant},
};

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
  drag: Rc<DragState>,
}

const RESTORE_DRAG_DISTANCE: f64 = 4.0;
const RESTORE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct DragState {
  gesture: Cell<Option<Gesture>>,
  maximized: Cell<bool>,
  drag_limit: Cell<f64>,
  button_width: Cell<f64>,
  minimum_width: Cell<f64>,
}

#[derive(Clone, Copy)]
enum Gesture {
  Moving(Drag),
  Maximized(RestoreAnchor),
  Restoring(RestoreDrag),
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

#[derive(Clone, Copy)]
struct RestoreAnchor {
  press: NSPoint,
  maximized_size: NSSize,
  horizontal_fraction: f64,
  button_width: f64,
  title_offset: f64,
}

impl RestoreAnchor {
  fn new(press: NSPoint, video: NSRect, panel: NSRect, drag_limit: f64, button_width: f64) -> Self {
    // Match the rectangle the eye can grab. A shrinking normal window
    // should not move that anchor into the fixed-width button strip.
    let title_width = drag_limit.min(panel.size.width);
    Self {
      press,
      maximized_size: video.size,
      horizontal_fraction: ((press.x - panel.min().x) / title_width).clamp(0.0, 1.0),
      button_width,
      title_offset: press.y - panel.min().y,
    }
  }

  fn begin_restore(self, pointer: NSPoint, released: bool) -> Option<RestoreDrag> {
    let moved = (pointer.x - self.press.x).hypot(pointer.y - self.press.y) >= RESTORE_DRAG_DISTANCE;
    (!released && moved).then(|| RestoreDrag {
      anchor: self,
      pointer,
      released: false,
      requested: Instant::now(),
    })
  }

  fn reanchor(self, pointer: NSPoint, size: NSSize) -> Drag {
    let title_width = (size.width - self.button_width).max(0.0);
    Drag {
      press: pointer,
      viewer_origin: NSPoint::new(
        pointer.x - title_width * self.horizontal_fraction,
        pointer.y - self.title_offset - size.height,
      ),
    }
  }
}

#[derive(Clone, Copy)]
struct RestoreDrag {
  anchor: RestoreAnchor,
  pointer: NSPoint,
  released: bool,
  requested: Instant,
}

impl RestoreDrag {
  fn restored(self, frame: NSRect, maximized: bool) -> Option<Drag> {
    // egui updates the viewport flag when it sends the AppKit zoom command.
    // Wait for native geometry as well before using the restored dimensions.
    (!maximized && frame.size != self.anchor.maximized_size && !frame.is_empty())
      .then(|| self.anchor.reanchor(self.pointer, frame.size))
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

fn move_viewer_and_panel(
  viewer: &NSWindow,
  panel: &NSWindow,
  origin: NSPoint,
  min_width: f64,
  main: MainThreadMarker,
) {
  let previous = panel.frame();
  if origin != viewer.frame().origin {
    viewer.setFrameOrigin(origin);
  }
  // AppKit moves a child along with its parent. If the viewer has no
  // attachment point, hold the controls in their last reachable place.
  let areas = work_areas(main);
  let frame = panel_frame(viewer.frame(), &areas, previous.size.height, min_width)
    .or_else(|| keep_panel_visible(previous, &areas, min_width))
    .unwrap_or(previous);
  place_panel(panel, frame);
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
    let drag = Rc::new(DragState {
      minimum_width: Cell::new(f64::from(min_width)),
      ..Default::default()
    });
    let monitor = monitor_drag(&self.window, &panel, &drag, ctx)?;
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
  drag: &Rc<DragState>,
  ctx: &egui::Context,
) -> Option<Retained<AnyObject>> {
  let mtm = MainThreadMarker::new()?;
  let viewer = viewer.clone();
  let panel = panel.clone();
  let drag = Rc::clone(drag);
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
        drag.gesture.set(None);
        let panel_frame = panel.frame();
        if report.windowNumber() != panel.windowNumber()
          || !drag_hit(location, panel_frame.size, drag.drag_limit.get())
        {
          return event.as_ptr();
        }
        let press = screen_point();
        let video = viewer.frame();
        let gesture = if drag.maximized.get() {
          Gesture::Maximized(RestoreAnchor::new(
            press,
            video,
            panel_frame,
            drag.drag_limit.get(),
            drag.button_width.get(),
          ))
        } else {
          Gesture::Moving(Drag {
            press,
            viewer_origin: video.origin,
          })
        };
        drag.gesture.set(Some(gesture));
      }
      NSEventType::LeftMouseDragged | NSEventType::LeftMouseUp => {
        let Some(gesture) = drag.gesture.get() else {
          return event.as_ptr();
        };
        let released = report.r#type() == NSEventType::LeftMouseUp;
        // Queued mouse events can still carry coordinates relative to the
        // panel's previous origin. Sample the pointer in screen coordinates.
        let pointer = NSEvent::mouseLocation();
        match gesture {
          Gesture::Moving(tracked) => {
            if released {
              drag.gesture.set(None);
            }
            move_viewer_and_panel(
              &viewer,
              &panel,
              tracked.viewer_at(pointer),
              drag.minimum_width.get(),
              mtm,
            );
          }
          Gesture::Maximized(anchor) => {
            if let Some(restoring) = anchor.begin_restore(pointer, released) {
              drag.gesture.set(Some(Gesture::Restoring(restoring)));
              ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Maximized(false));
            } else if released {
              drag.gesture.set(None);
            }
          }
          Gesture::Restoring(mut restoring) => {
            restoring.pointer = pointer;
            restoring.released |= released;
            drag.gesture.set(Some(Gesture::Restoring(restoring)));
          }
        }
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
    self.drag.gesture.get().is_some()
  }

  pub(super) fn sync_drag(&self, maximized: bool) {
    self.drag.maximized.set(maximized);
    let Some(Gesture::Restoring(restoring)) = self.drag.gesture.get() else {
      return;
    };
    let window = &self.viewer.window;
    if let Some(tracked) = restoring.restored(window.frame(), maximized)
      && let Some(main) = MainThreadMarker::new()
    {
      self
        .drag
        .gesture
        .set((!restoring.released).then_some(Gesture::Moving(tracked)));
      move_viewer_and_panel(
        window,
        &self.panel,
        tracked.viewer_origin,
        self.drag.minimum_width.get(),
        main,
      );
    } else if restoring.requested.elapsed() >= RESTORE_TIMEOUT {
      // An acknowledged zoom may leave an already work-area-sized normal
      // frame unchanged. Rebase without moving it or holding the bar open.
      self
        .drag
        .gesture
        .set((!maximized && !restoring.released).then(|| {
          Gesture::Moving(Drag {
            press: restoring.pointer,
            viewer_origin: window.frame().origin,
          })
        }));
    }
  }

  pub(super) fn width(&self) -> f64 {
    self.panel.frame().size.width
  }

  /// The latest painted title divider and button-strip width in native points.
  pub(super) fn set_drag_bounds(&self, right_edge: f64, button_width: f64) {
    self.drag.drag_limit.set(right_edge);
    self.drag.button_width.set(button_width);
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
    self.drag.minimum_width.set(f64::from(min_width));
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
  fn clicking_a_maximized_title_does_not_request_restore() {
    let video = rect(0.0, 0.0, 1200.0, 800.0);
    let panel = rect(0.0, 766.0, 1200.0, 34.0);
    let anchor = RestoreAnchor::new(NSPoint::new(250.0, 783.0), video, panel, 1028.0, 172.0);
    assert!(anchor.begin_restore(anchor.press, true).is_none());
    assert!(
      anchor
        .begin_restore(NSPoint::new(252.0, 781.0), false)
        .is_none()
    );
    assert!(
      anchor
        .begin_restore(NSPoint::new(250.0, 760.0), true)
        .is_none()
    );
    assert!(
      anchor
        .begin_restore(NSPoint::new(250.0, 778.0), false)
        .is_some()
    );
  }

  #[test]
  fn restored_drag_waits_for_normal_geometry_and_uses_the_title_anchor() {
    let video = rect(-800.0, 40.0, 1720.0, 820.0);
    let panel = rect(-800.0, 826.0, 1720.0, 34.0);
    let anchor = RestoreAnchor::new(NSPoint::new(740.0, 844.0), video, panel, 1548.0, 172.0);
    let pointer = NSPoint::new(708.0, 802.0);
    let restoring = anchor.begin_restore(pointer, false).unwrap();
    let normal = rect(120.0, 200.0, 640.0, 360.0);
    assert!(restoring.restored(video, false).is_none());
    assert!(
      restoring
        .restored(NSRect::new(NSPoint::ZERO, video.size), false)
        .is_none()
    );
    assert!(restoring.restored(normal, true).is_none());

    let track = restoring.restored(normal, false).unwrap();
    let offset = pointer.x - track.viewer_origin.x;
    assert!(offset > 460.0 && offset < 468.0);
    assert_eq!(track.viewer_origin.y + normal.size.height, pointer.y - 18.0);
    let later = track.viewer_at(NSPoint::new(pointer.x - 13.0, pointer.y - 21.0));
    assert_eq!(later.x - track.viewer_origin.x, -13.0);
    assert_eq!(later.y - track.viewer_origin.y, -21.0);
  }

  #[test]
  fn restoring_during_native_widening_uses_the_painted_button_width() {
    let window = rect(0.0, 0.0, 1200.0, 800.0);
    let panel = rect(0.0, 766.0, 1200.0, 34.0);
    // The panel is already maximized, but its last paint still covered only
    // 640 points. The 560 unpainted points are not part of the button strip.
    let anchor = RestoreAnchor::new(NSPoint::new(351.0, 783.0), window, panel, 468.0, 172.0);
    let pointer = NSPoint::new(500.0, 750.0);
    let drag = anchor.reanchor(pointer, NSSize::new(640.0, 360.0));
    assert_eq!(pointer.x - drag.viewer_origin.x, 351.0);
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
