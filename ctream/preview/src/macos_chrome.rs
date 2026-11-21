use super::chrome::{CONTROLS_TITLE, VIEWER_TITLE};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
  NSApplication, NSEvent, NSEventType, NSWindow, NSWindowCollectionBehavior, NSWindowOrderingMode,
};
use objc2_foundation::{NSContainsRect, NSPoint, NSPointInRect, NSRect, NSSize, NSString};

const GAP: f64 = 4.0;

pub(super) struct ViewerWindow {
  application: Retained<NSApplication>,
  window: Retained<NSWindow>,
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

fn panel_frame(video: NSRect, work_area: NSRect, height: f64) -> Option<NSRect> {
  if video.is_empty() || work_area.is_empty() || height <= 0.0 {
    return None;
  }
  let width = video.size.width.min(work_area.size.width);
  let x = video
    .min()
    .x
    .clamp(work_area.min().x, work_area.max().x - width);
  for y in [video.max().y + GAP, video.min().y - GAP - height] {
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    if NSContainsRect(work_area, frame) {
      return Some(frame);
    }
  }
  None
}

impl ViewerWindow {
  pub(super) fn active(&self) -> bool {
    self.application.isActive() && self.window.isVisible() && !self.window.isMiniaturized()
  }

  pub(super) fn panel_frame(&self, height: f32) -> Option<NSRect> {
    panel_frame(
      self.window.frame(),
      self.window.screen()?.visibleFrame(),
      f64::from(height),
    )
  }

  pub(super) fn can_attach(&self, height: f32) -> bool {
    self.panel_frame(height).is_some()
  }

  pub(super) fn hovering(&self, shown: bool, height: f32, reveal: f32) -> bool {
    if !self.active() {
      return false;
    }
    let Some(panel) = self.panel_frame(height) else {
      return false;
    };
    let pointer = NSEvent::mouseLocation();
    let video = self.window.frame();
    let above = panel.min().y > video.max().y;
    if NSPointInRect(pointer, video) {
      return (shown && !above) || pointer.y >= video.max().y - f64::from(reveal);
    }
    if !shown {
      return false;
    }
    // Keep the panel while the pointer crosses the small gap between windows.
    // A panel placed below the viewer stays open as the pointer crosses the
    // video from the top-edge reveal strip to the panel.
    let origin = NSPoint::new(
      panel.min().x,
      if above { video.max().y } else { panel.min().y },
    );
    let bridge = NSRect::new(
      origin,
      NSSize::new(panel.size.width, panel.size.height + GAP),
    );
    NSPointInRect(pointer, bridge)
  }

  pub(super) fn attach_controls(&self, height: f32) -> bool {
    let Some(frame) = self.panel_frame(height) else {
      return false;
    };
    let Some(panel) = own_window(&self.application, CONTROLS_TITLE) else {
      return false;
    };
    if panel.frame() != frame {
      panel.setFrame_display(frame, false);
    }
    if panel.parentWindow().is_none() {
      panel.setExcludedFromWindowsMenu(true);
      panel.setCollectionBehavior(
        NSWindowCollectionBehavior::Transient
          | NSWindowCollectionBehavior::IgnoresCycle
          | NSWindowCollectionBehavior::FullScreenAuxiliary,
      );
      panel.setHidesOnDeactivate(true);
      // Both windows belong to our application, have distinct titles, and
      // only the viewer can be the parent.
      unsafe {
        self
          .window
          .addChildWindow_ordered(&panel, NSWindowOrderingMode::Above);
      }
    }
    if !panel.isVisible() && self.active() {
      panel.orderFront(None);
    }
    true
  }

  pub(super) fn restore_focus(&self) {
    if self.active()
      && own_window(&self.application, CONTROLS_TITLE).is_some_and(|panel| panel.isKeyWindow())
    {
      self.window.makeKeyWindow();
    }
  }

  pub(super) fn start_drag(&self) {
    let Some(event) = self.application.currentEvent() else {
      return;
    };
    if event.r#type() == NSEventType::LeftMouseDown
      && own_window(&self.application, CONTROLS_TITLE)
        .is_some_and(|panel| event.windowNumber() == panel.windowNumber())
    {
      self.window.performWindowDragWithEvent(&event);
    }
  }
}

pub(super) fn hide_controls() {
  let Some(main_thread) = MainThreadMarker::new() else {
    return;
  };
  let application = NSApplication::sharedApplication(main_thread);
  let Some(panel) = own_window(&application, CONTROLS_TITLE) else {
    return;
  };
  let panel_was_key = panel.isKeyWindow();
  let parent = panel.parentWindow();
  if let Some(parent) = &parent {
    parent.removeChildWindow(&panel);
  }
  if panel.isVisible() {
    panel.orderOut(None);
  }
  if panel_was_key
    && application.isActive()
    && let Some(parent) = parent.filter(|window| window.isVisible() && !window.isMiniaturized())
  {
    parent.makeKeyWindow();
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
      Some(rect(-720.0, 544.0, 640.0, 34.0))
    );
  }

  #[test]
  fn title_panel_uses_visible_space_below_when_the_top_is_blocked() {
    let desktop = rect(0.0, 40.0, 800.0, 560.0);
    let video = rect(100.0, 200.0, 640.0, 400.0);
    assert_eq!(
      panel_frame(video, desktop, 34.0),
      Some(rect(100.0, 162.0, 640.0, 34.0))
    );
    assert_eq!(panel_frame(desktop, desktop, 34.0), None);
  }
}
