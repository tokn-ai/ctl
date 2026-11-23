use ctream_client::geometry::{ImageGeometry, Point, Rect, ViewportSpace};
use eframe::egui;
use std::{
  sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, RecvTimeoutError, SyncSender},
  },
  thread,
  time::Duration,
};

pub(super) trait WindowState {
  fn is_viewing(&self) -> bool;
  fn logic(&mut self, ctx: &egui::Context);
  fn setup(&mut self, ui: &mut egui::Ui) -> Option<egui::ViewportBuilder>;
  fn viewer(&mut self, ui: &mut egui::Ui, class: egui::ViewportClass);
}

pub(super) struct App<V> {
  state: Arc<Mutex<V>>,
}

impl<V: WindowState + Send + 'static> App<V> {
  pub(super) fn new(state: V) -> Self {
    Self {
      state: Arc::new(Mutex::new(state)),
    }
  }

  pub(super) fn is_viewing(&self) -> bool {
    self.state.lock().unwrap().is_viewing()
  }

  pub(super) fn logic(&mut self, ctx: &egui::Context) {
    self.state.lock().unwrap().logic(ctx);
  }

  pub(super) fn update_ui(&mut self, ui: &mut egui::Ui) {
    let builder = self.state.lock().unwrap().setup(ui);
    if let Some(builder) = builder {
      // The viewer needs its own redraws while its setup window is hidden.
      // The app owns the session so a retained viewport callback cannot keep it alive.
      let state = Arc::downgrade(&self.state);
      ui.ctx()
        .show_viewport_deferred(viewer_id(), builder, move |ui, class| {
          if let Some(state) = state.upgrade() {
            state.lock().unwrap().viewer(ui, class);
          }
        });
    }
  }
}

impl<V: WindowState + Send + 'static> eframe::App for App<V> {
  fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    self.logic(ctx);
  }

  fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
    self.update_ui(ui);
  }
}

#[derive(Clone, Default)]
pub(super) struct Repaint(Arc<AtomicBool>);

impl Repaint {
  pub(super) fn set_viewing(&self, viewing: bool) {
    self.0.store(viewing, Ordering::Relaxed);
  }

  fn viewport(&self) -> egui::ViewportId {
    if self.0.load(Ordering::Relaxed) {
      viewer_id()
    } else {
      egui::ViewportId::ROOT
    }
  }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum PendingCapture {
  Idle,
  Frame,
  Ended,
}

impl PendingCapture {
  pub(super) fn new(has_frame: bool, ended: bool) -> Self {
    if ended {
      Self::Ended
    } else if has_frame {
      Self::Frame
    } else {
      Self::Idle
    }
  }
}

/// Keep the capture reader independent of the egui context lock. One pending
/// wake is enough because the current viewport consumes only the latest frame.
pub(super) fn capture_repaint(
  ctx: &egui::Context,
  repaint: Repaint,
  capture: impl Fn() -> PendingCapture + Send + 'static,
) -> SyncSender<()> {
  let (wake, pending) = mpsc::sync_channel(1);
  let ctx = ctx.clone();
  thread::spawn(move || {
    let mut retry = false;
    #[cfg(target_os = "macos")]
    let native_redraw_queued = Arc::new(AtomicBool::new(false));
    loop {
      let notified = if retry {
        pending.recv_timeout(Duration::from_millis(250))
      } else {
        pending.recv().map_err(|_| RecvTimeoutError::Disconnected)
      };
      let viewport = repaint.viewport();
      match notified {
        Ok(()) => {
          ctx.request_repaint_of(viewport);
          let capture = capture();
          // Setup can handle capture end even if the viewer is minimized.
          if viewport != egui::ViewportId::ROOT && capture == PendingCapture::Ended {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
          }
          retry = cfg!(target_os = "macos")
            && viewport == viewer_id()
            && capture == PendingCapture::Frame;
        }
        Err(RecvTimeoutError::Timeout) => {
          retry = viewport == viewer_id() && capture() == PendingCapture::Frame;
          #[cfg(target_os = "macos")]
          if retry && !native_redraw_queued.swap(true, Ordering::Relaxed) {
            // A missed native redraw can leave egui coalescing later requests
            // into one already consumed by eframe. Rearm the visible window.
            let queued = Arc::clone(&native_redraw_queued);
            let repaint = repaint.clone();
            dispatch2::DispatchQueue::main().exec_async(move || {
              if repaint.viewport() == viewer_id() {
                super::macos_chrome::redraw_visible_viewer();
              }
              queued.store(false, Ordering::Relaxed);
            });
          }
        }
        Err(RecvTimeoutError::Disconnected) => break,
      }
    }
  });
  wake
}

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

pub(super) fn upload(
  ctx: &egui::Context,
  texture: &mut Option<egui::TextureHandle>,
  image: egui::ColorImage,
) {
  if let Some(texture) = texture {
    texture.set(image, egui::TextureOptions::LINEAR);
  } else {
    *texture = Some(ctx.load_texture("capture", image, egui::TextureOptions::LINEAR));
  }
}

pub(super) fn paint(
  ui: &mut egui::Ui,
  texture: &egui::TextureHandle,
  area: egui::Rect,
  image: &ImageGeometry,
) -> egui::Rect {
  // Painting alone does not occupy layout space.
  ui.allocate_rect(area, egui::Sense::hover());
  let rect = rect(image.viewport());
  ui.painter().image(
    texture.id(),
    rect,
    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
    egui::Color32::WHITE,
  );
  rect
}

pub const MIN_WINDOW: egui::Vec2 = egui::vec2(200.0, 140.0);
const PREFERRED_WINDOW: egui::Vec2 = egui::vec2(1100.0, 720.0);
const RESIZE_SETTLE: Duration = Duration::from_millis(120);

pub(super) fn viewer_id() -> egui::ViewportId {
  egui::ViewportId::from_hash_of("ctream_viewer")
}

pub(super) fn viewer_builder(
  ctx: &egui::Context,
  initial: Option<([u32; 2], f32)>,
) -> egui::ViewportBuilder {
  let builder = egui::ViewportBuilder::default()
    .with_title(super::chrome::VIEWER_TITLE)
    .with_decorations(false)
    .with_min_inner_size(MIN_WINDOW)
    .with_active(true);
  if let Some((image, sidebar)) = initial {
    let monitor = ctx.input(|input| input.viewport().monitor_size);
    builder.with_inner_size(initial_window(image, sidebar, monitor))
  } else {
    builder
  }
}

pub(super) fn hide_setup(ctx: &egui::Context) {
  ctx.send_viewport_cmd_to(
    egui::ViewportId::ROOT,
    egui::ViewportCommand::Visible(false),
  );
  ctx.send_viewport_cmd_to(viewer_id(), egui::ViewportCommand::Focus);
}

pub(super) fn restore_setup(ctx: &egui::Context) {
  ctx.send_viewport_cmd_to(viewer_id(), egui::ViewportCommand::Visible(false));
  ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
  ctx.send_viewport_cmd_to(
    egui::ViewportId::ROOT,
    egui::ViewportCommand::Minimized(false),
  );
  ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
  ctx.request_repaint_of(egui::ViewportId::ROOT);
}

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
