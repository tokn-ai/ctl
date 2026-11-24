#[cfg(target_os = "macos")]
use dispatch2::MainThreadBound;
use eframe::egui::{self, Align, Pos2, Rect, ViewportCommand};
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use std::time::Duration;

pub(super) const REVEAL_HEIGHT: f32 = 8.0;
pub(super) const BAR_HEIGHT: f32 = 34.0;
pub(super) const BUTTON_WIDTH: f32 = 40.0;
const BUTTON_STRIP_WIDTH: f32 = 3.0 * BUTTON_WIDTH;
const DEBUG_BUTTON_WIDTH: f32 = 52.0;
#[cfg(target_os = "macos")]
const MIN_DRAG_WIDTH: f32 = 28.0;
pub(super) const VIEWER_TITLE: &str = "ctream viewer";
#[cfg(any(target_os = "macos", test))]
pub(super) const CONTROLS_TITLE: &str = "ctream viewer controls";
#[cfg(any(target_os = "linux", windows))]
pub(super) const RESIZE_GRIP: f32 = 14.0;

#[derive(Clone, Copy)]
pub(super) struct Status<'a> {
  pub text: &'a str,
  pub error: bool,
  pub driver: Option<&'a str>,
  pub debug: Option<bool>,
}

impl Status<'_> {
  fn button_strip_width(self) -> f32 {
    BUTTON_STRIP_WIDTH
      + if self.debug.is_some() {
        DEBUG_BUTTON_WIDTH
      } else {
        0.0
      }
  }
}

pub(super) struct Frame {
  pub pointer_regions: [Rect; 2],
  pub close: bool,
  pub toggle_debug: bool,
}

impl Frame {
  fn new(toolbar: Rect, resize: Rect) -> Self {
    Self {
      pointer_regions: [toolbar, resize],
      close: false,
      toggle_debug: false,
    }
  }

  fn apply(mut self, ctx: &egui::Context, action: Option<Action>) -> Self {
    self.close = action.is_some_and(|action| action.apply(ctx));
    self.toggle_debug = matches!(action, Some(Action::ToggleDebug));
    self
  }
}

#[derive(Default)]
pub(super) struct Chrome {
  overlay_shown: bool,
  #[cfg(target_os = "macos")]
  attached: Option<MainThreadBound<super::macos_chrome::AttachedPanel>>,
  #[cfg(test)]
  pub test_attached_hover: Option<bool>,
  #[cfg(test)]
  pub test_attached_overlap: Option<Rect>,
}

impl Chrome {
  pub fn show(&mut self, ui: &mut egui::Ui, status: Status<'_>) -> Frame {
    let ctx = ui.ctx().clone();
    let window = ctx.content_rect();
    #[cfg(test)]
    if let Some(hovered) = self.test_attached_hover {
      let action = hovered
        .then(|| show_controls(&ctx, status, true, window.width(), true).action)
        .flatten();
      let overlap = if hovered {
        self
          .test_attached_overlap
          .unwrap_or(Rect::NOTHING)
          .intersect(window)
      } else {
        Rect::NOTHING
      };
      return Frame::new(overlap, Rect::NOTHING).apply(&ctx, action);
    }
    let (hovered, pointer_down, released) = ctx.input(|input| {
      (
        input.pointer.hover_pos(),
        input.pointer.any_down(),
        input.pointer.any_released(),
      )
    });
    // Keep the same hit regions until release, including input batches that
    // contain both a press and its release.
    let gesture_active = pointer_down || released;
    if released {
      ctx.request_repaint();
    }
    #[cfg(target_os = "macos")]
    {
      if !self.overlay_shown
        && let Some(frame) = self.show_macos(&ctx, status, hovered, gesture_active)
      {
        return frame;
      }
      self.attached.take();
    }
    let reveal = Rect::from_min_size(window.min, egui::vec2(window.width(), REVEAL_HEIGHT));
    let bar = Rect::from_min_size(window.min, egui::vec2(window.width(), BAR_HEIGHT));
    if !gesture_active {
      let hover_zone = if self.overlay_shown { bar } else { reveal };
      self.overlay_shown = hovered.is_some_and(|pos| hover_zone.contains(pos));
    }
    let resize = resize_grip(&ctx, window, hovered);
    if !self.overlay_shown {
      return Frame::new(Rect::NOTHING, resize);
    }
    let bar = bar.intersect(window);
    let action = egui::Area::new(egui::Id::unique("ctream_title_overlay"))
      .order(egui::Order::Foreground)
      .fixed_pos(bar.min)
      .default_size(bar.size())
      .constrain(false)
      .fade_in(false)
      .show(&ctx, |ui| title_bar(ui, bar, status, maximized(&ctx), true))
      .inner;
    Frame::new(bar, resize).apply(&ctx, action)
  }

  pub fn dismiss(&mut self) {
    self.overlay_shown = false;
    #[cfg(target_os = "macos")]
    self.attached.take();
  }

  #[cfg(target_os = "macos")]
  fn show_macos(
    &mut self,
    ctx: &egui::Context,
    status: Status<'_>,
    hovered: Option<Pos2>,
    gesture_active: bool,
  ) -> Option<Frame> {
    let main = MainThreadMarker::new()?;
    let viewer = super::macos_chrome::viewer()?;
    let zoom = ctx.zoom_factor();
    let height = BAR_HEIGHT * zoom;
    let reveal = REVEAL_HEIGHT * zoom;
    let button_width = status.button_strip_width();
    let min_width = (button_width + MIN_DRAG_WIDTH) * zoom;
    if !viewer.active() {
      self.dismiss();
      return Some(Frame::new(Rect::NOTHING, Rect::NOTHING));
    }
    let opening = self.attached.is_none();
    let width = if let Some(panel) = &self.attached {
      let panel = panel.get(main);
      panel.sync_drag(maximized(ctx));
      if !panel.dragging() && !gesture_active {
        if !panel.hovering(reveal) {
          self.dismiss();
          return Some(Frame::new(Rect::NOTHING, Rect::NOTHING));
        }
        if !panel.place(height, min_width) {
          return None;
        }
      }
      panel.width()
    } else {
      if gesture_active || hovered.is_none() {
        return Some(Frame::new(Rect::NOTHING, Rect::NOTHING));
      }
      let width = viewer.panel_width(height, min_width)?;
      if !viewer.reveal_hovering(height, min_width, reveal) {
        return Some(Frame::new(Rect::NOTHING, Rect::NOTHING));
      }
      width
    };

    let controls = show_controls(ctx, status, false, width as f32 / zoom, opening);
    if opening {
      let panel = viewer.attach_controls(height, min_width, ctx)?;
      self.attached = Some(MainThreadBound::new(panel, main));
    }
    let panel = self.attached.as_ref()?.get(main);
    panel.sync_drag(maximized(ctx));
    panel.set_drag_bounds(
      f64::from(controls.drag_limit) * f64::from(zoom),
      f64::from(button_width) * f64::from(zoom),
    );
    let action = controls.action;
    let frame =
      Frame::new(panel.overlay_rect(ctx.content_rect()), Rect::NOTHING).apply(ctx, action);
    if frame.close || matches!(action, Some(Action::Minimize)) {
      self.dismiss();
    } else {
      if action.is_some() {
        panel.restore_focus();
      }
      // Leaving both native windows may produce no further pointer events.
      ctx.request_repaint_after(Duration::from_millis(33));
    }
    Some(frame)
  }
}

fn primary_press_in(ctx: &egui::Context, bounds: Rect) -> bool {
  ctx.current_pass_index() == 0
    && ctx.input(|input| {
      input.raw.events.iter().any(|event| {
        matches!(
          event,
          egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            ..
          } if bounds.contains(*pos)
        )
      })
    })
}

#[derive(Clone, Copy)]
enum Action {
  Drag,
  Minimize,
  Maximize(bool),
  ToggleDebug,
  Close,
}

impl Action {
  fn apply(self, ctx: &egui::Context) -> bool {
    let command = match self {
      Self::Drag => ViewportCommand::StartDrag,
      Self::Minimize => ViewportCommand::Minimized(true),
      Self::Maximize(maximized) => ViewportCommand::Maximized(maximized),
      Self::ToggleDebug => return false,
      Self::Close => return true,
    };
    ctx.send_viewport_cmd(command);
    false
  }
}

fn maximized(ctx: &egui::Context) -> bool {
  ctx.input(|input| input.viewport().maximized == Some(true))
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn controls_id() -> egui::ViewportId {
  egui::ViewportId::from_hash_of("ctream_floating_controls")
}

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct ControlsFrame {
  action: Option<Action>,
  #[cfg(target_os = "macos")]
  drag_limit: f32,
}

#[cfg(any(target_os = "macos", test))]
fn show_controls(
  ctx: &egui::Context,
  status: Status<'_>,
  draggable: bool,
  width: f32,
  opening: bool,
) -> ControlsFrame {
  let maximized = maximized(ctx);
  let mut builder = egui::ViewportBuilder::default()
    .with_title(CONTROLS_TITLE)
    .with_decorations(false)
    .with_resizable(false)
    .with_active(false)
    .with_visible(false);
  // AppKit owns placement and size once the panel is attached.
  if opening {
    builder = builder
      .with_clamp_size_to_monitor_size(false)
      .with_inner_size([width, BAR_HEIGHT]);
  }
  ctx.show_viewport_immediate(controls_id(), builder, |panel, class| {
    if class == egui::ViewportClass::EmbeddedWindow {
      return ControlsFrame::default();
    }
    if panel.input(|input| input.viewport().close_requested()) {
      return ControlsFrame {
        action: Some(Action::Close),
        #[cfg(target_os = "macos")]
        drag_limit: 0.0,
      };
    }
    let mut bounds = panel.ctx().content_rect();
    let window_left = panel.ctx().viewport_rect().left();
    // AppKit changes the panel synchronously. The child viewport's next
    // layout may still reflect its previous width during a drag.
    bounds.max.x = bounds.max.x.min(window_left + width);
    if bounds.width() < status.button_strip_width() {
      return ControlsFrame::default();
    }
    ControlsFrame {
      action: title_bar(panel, bounds, status, maximized, draggable),
      // Use the rendered button edge for native mouse events as well.
      #[cfg(target_os = "macos")]
      drag_limit: buttons_left(bounds, status) - window_left,
    }
  })
}

fn buttons_left(bounds: Rect, status: Status<'_>) -> f32 {
  bounds.right() - status.button_strip_width()
}

fn title_bar(
  ui: &mut egui::Ui,
  bounds: Rect,
  status: Status<'_>,
  maximized: bool,
  draggable: bool,
) -> Option<Action> {
  let buttons_left = buttons_left(bounds, status);
  let title = Rect::from_min_max(bounds.min, egui::pos2(buttons_left, bounds.bottom()));
  let dragging = draggable && primary_press_in(ui.ctx(), title);
  ui.set_min_size(bounds.size());
  ui.set_clip_rect(bounds.intersect(ui.clip_rect()));
  ui.painter()
    .rect_filled(bounds, 0.0, ui.visuals().panel_fill);
  ui.painter().hline(
    bounds.x_range(),
    bounds.bottom(),
    ui.visuals().widgets.noninteractive.bg_stroke,
  );
  ui.scope_builder(
    egui::UiBuilder::new()
      .max_rect(title.shrink2(egui::vec2(10.0, 0.0)))
      .layout(egui::Layout::left_to_right(Align::Center)),
    |ui| show_status(ui, status),
  );
  if control(ui, bounds, 2, "−", "Minimize") {
    return Some(Action::Minimize);
  }
  let (icon, label) = if maximized {
    ("▣", "Restore")
  } else {
    ("□", "Maximize")
  };
  if control(ui, bounds, 1, icon, label) {
    return Some(Action::Maximize(!maximized));
  }
  if control(ui, bounds, 0, "×", "Close viewer") {
    return Some(Action::Close);
  }
  if let Some(enabled) = status.debug {
    let debug = ui
      .put(
        button_slot(
          bounds,
          bounds.right() - BUTTON_STRIP_WIDTH,
          DEBUG_BUTTON_WIDTH,
        ),
        egui::Button::new("Debug").selected(enabled),
      )
      .on_hover_text(if enabled {
        "Hide debug overlay"
      } else {
        "Show debug overlay"
      });
    if debug.clicked() {
      debug.surrender_focus();
      return Some(Action::ToggleDebug);
    }
  }
  dragging.then_some(Action::Drag)
}

fn show_status(ui: &mut egui::Ui, status: Status<'_>) {
  ui.add(egui::Label::new(egui::RichText::new(VIEWER_TITLE).strong()).truncate());
  let mut text = egui::RichText::new(status.text).small();
  if status.error {
    text = text.color(ui.visuals().error_fg_color);
  } else {
    text = text.weak();
  }
  ui.add(egui::Label::new(text).truncate())
    .on_hover_text(status.text);
  if let Some(driver) = status.driver {
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
      ui.add(egui::Label::new(egui::RichText::new(driver).small()).truncate())
        .on_hover_text(driver);
    });
  }
}

fn button_slot(bounds: Rect, right: f32, width: f32) -> Rect {
  Rect::from_min_size(
    egui::pos2(right - width, bounds.top()),
    egui::vec2(width, BAR_HEIGHT),
  )
  .shrink2(egui::vec2(2.0, 3.0))
}

fn control(ui: &mut egui::Ui, bounds: Rect, index: u8, icon: &str, label: &str) -> bool {
  let right = bounds.right() - f32::from(index) * BUTTON_WIDTH;
  ui.put(
    button_slot(bounds, right, BUTTON_WIDTH),
    egui::Button::new(egui::RichText::new(icon).size(17.0)),
  )
  .on_hover_text(label)
  .clicked()
}

#[cfg(any(target_os = "linux", windows))]
fn resize_grip(ctx: &egui::Context, window: Rect, hovered: Option<Pos2>) -> Rect {
  let fixed = ctx.input(|input| {
    let viewport = input.viewport();
    viewport.maximized == Some(true) || viewport.fullscreen == Some(true)
  });
  if fixed {
    return Rect::NOTHING;
  }
  let grip = Rect::from_min_max(window.max - egui::Vec2::splat(RESIZE_GRIP), window.max);
  if hovered.is_some_and(|pos| grip.contains(pos)) {
    ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    let painter = ctx.layer_painter(egui::LayerId::new(
      egui::Order::Foreground,
      egui::Id::unique("ctream_resize_grip"),
    ));
    painter.rect_filled(grip, 0.0, ctx.global_style().visuals.panel_fill);
    for inset in [6.0, 10.0] {
      let corner = grip.max - egui::Vec2::splat(3.0);
      painter.line_segment(
        [
          corner - egui::vec2(inset, 0.0),
          corner - egui::vec2(0.0, inset),
        ],
        egui::Stroke::new(1.0_f32, ctx.global_style().visuals.text_color()),
      );
    }
  }
  if primary_press_in(ctx, grip) {
    ctx.send_viewport_cmd(ViewportCommand::BeginResize(
      egui::ResizeDirection::SouthEast,
    ));
  }
  grip
}

#[cfg(not(any(target_os = "linux", windows)))]
fn resize_grip(_ctx: &egui::Context, _window: Rect, _hovered: Option<Pos2>) -> Rect {
  // AppKit exposes native resize edges on resizable borderless windows.
  Rect::NOTHING
}
