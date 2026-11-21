use ctream_client::geometry::{ImageGeometry, Point, Rect, ViewportSpace};
use eframe::egui;
use std::time::Duration;

fn viewport(area: egui::Rect) -> Option<Rect<ViewportSpace>> {
  Rect::new(
    Point::new(f64::from(area.min.x), f64::from(area.min.y)),
    Point::new(f64::from(area.max.x), f64::from(area.max.y)),
  )
}

pub fn fit(area: egui::Rect, size: [u32; 2]) -> Option<ImageGeometry> {
  ImageGeometry::fit(viewport(area)?, size)
}

pub fn fill(area: egui::Rect, size: [u32; 2]) -> Option<ImageGeometry> {
  ImageGeometry::fill(viewport(area)?, size)
}

pub fn rect(rect: Rect<ViewportSpace>) -> egui::Rect {
  egui::Rect::from_min_max(
    egui::pos2(rect.min().x as f32, rect.min().y as f32),
    egui::pos2(rect.max().x as f32, rect.max().y as f32),
  )
}

pub const MIN_WINDOW: egui::Vec2 = egui::vec2(200.0, 140.0);
const PREFERRED_WINDOW: egui::Vec2 = egui::vec2(1100.0, 720.0);
const RESIZE_SETTLE: Duration = Duration::from_millis(120);

fn aspect([width, height]: [u32; 2]) -> Option<f32> {
  (width > 0 && height > 0).then(|| width as f32 / height as f32)
}

fn monitor_limit(monitor: Option<egui::Vec2>) -> egui::Vec2 {
  // Leave space for the taskbar or dock when the monitor size includes it.
  monitor.map_or(egui::Vec2::INFINITY, |size| size * 0.94)
}

fn sized_window(aspect: f32, chrome: egui::Vec2, limit: egui::Vec2, width: f32) -> egui::Vec2 {
  let max = (limit - chrome).max(egui::Vec2::splat(1.0));
  let min = (MIN_WINDOW - chrome).max(egui::Vec2::splat(1.0));
  let max_width = max.x.min(max.y * aspect);
  let min_width = min.x.max(min.y * aspect).min(max_width);
  let width = width.clamp(min_width, max_width);
  egui::vec2(width, width / aspect) + chrome
}

pub fn initial_window(
  source: [u32; 2],
  sidebar_width: f32,
  monitor: Option<egui::Vec2>,
) -> egui::Vec2 {
  let limit = PREFERRED_WINDOW.min(monitor_limit(monitor));
  aspect(source).map_or(limit, |aspect| {
    sized_window(aspect, egui::vec2(sidebar_width, 0.0), limit, f32::INFINITY)
  })
}

#[derive(PartialEq)]
struct Sample {
  video: egui::Vec2,
  chrome: egui::Vec2,
  aspect: f32,
  limit: egui::Vec2,
}

#[derive(Default)]
pub struct AspectResize {
  observed: Option<Sample>,
  matching: Option<egui::Vec2>,
  deadline: f64,
  requested: Option<egui::Vec2>,
}

impl AspectResize {
  pub fn update(&mut self, ctx: &egui::Context, source: [u32; 2], video: egui::Vec2) {
    let Some(aspect) = aspect(source) else { return };
    if video.x <= 0.0 || video.y <= 0.0 {
      return;
    }
    let (now, monitor, constrained) = ctx.input(|input| {
      let viewport = input.viewport();
      let constrained =
        [viewport.fullscreen, viewport.maximized, viewport.minimized].contains(&Some(true));
      (input.time, viewport.monitor_size, constrained)
    });
    if constrained {
      *self = Self::default();
      return;
    }
    let window = ctx.viewport_rect().size();
    let chrome = (window - video).max(egui::Vec2::ZERO);
    let limit = monitor_limit(monitor);
    let sample = Sample {
      video,
      chrome,
      aspect,
      limit,
    };
    if self.observed.as_ref() != Some(&sample) {
      self.observed = Some(sample);
      self.deadline = now + RESIZE_SETTLE.as_secs_f64();
    }

    if (video.y - video.x / aspect).abs() <= 1.0 || (video.x - video.y * aspect).abs() <= 1.0 {
      self.matching = Some(video);
      self.requested = None;
      return;
    }
    if now < self.deadline {
      ctx.request_repaint_after(Duration::from_secs_f64(self.deadline - now));
      return;
    }

    // Preserve the dimension the user changed most. If just the source aspect
    // changed, keep the current width so the viewport does not jump in scale.
    let width = if self.matching.is_some_and(|previous| {
      let change = (video / previous - egui::Vec2::splat(1.0)).abs();
      change.y > change.x
    }) {
      video.y * aspect
    } else {
      video.x
    };
    let desired = sized_window(aspect, chrome, limit, width);
    if self.requested != Some(desired) && (desired - window).abs().max_elem() > 1.0 {
      self.requested = Some(desired);
      ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(desired));
    }
  }
}
