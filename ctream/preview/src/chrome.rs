#[cfg(target_os = "macos")]
use dispatch2::MainThreadBound;
use eframe::egui::{self, Align, Pos2, Rect, ViewportCommand};
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
use std::time::Duration;

pub(super) const REVEAL_HEIGHT: f32 = 8.0;
pub(super) const BAR_HEIGHT: f32 = 34.0;
pub(super) const BUTTON_WIDTH: f32 = 40.0;
const BUTTON_STRIP_WIDTH: f32 = 3.0 * BUTTON_WIDTH;
const DEBUG_BUTTON_WIDTH: f32 = 52.0;
pub(super) const VIEWER_TITLE: &str = "ctream viewer";
#[cfg(any(target_os = "macos", test))]
pub(super) const CONTROLS_TITLE: &str = "ctream viewer controls";
const RESIZE_WAIT: Duration = Duration::from_millis(120);
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
  pub resizing: bool,
}

impl Frame {
  #[cfg(any(target_os = "macos", test))]
  fn attached() -> Self {
    Self {
      pointer_regions: [Rect::NOTHING; 2],
      close: false,
      toggle_debug: false,
      resizing: false,
    }
  }

  fn apply(mut self, ctx: &egui::Context, action: Option<Action>) -> Self {
    self.close = action.is_some_and(|action| action.apply(ctx));
    self.toggle_debug = matches!(action, Some(Action::ToggleDebug));
    self
  }
}

struct Resize {
  shown: bool,
  before: f32,
  target: f32,
  deadline: f64,
}

#[derive(Default)]
pub(super) struct Chrome {
  shown: bool,
  allocated: f32,
  fixed: bool,
  resize: Option<Resize>,
  #[cfg(target_os = "macos")]
  attached: Option<MainThreadBound<super::macos_chrome::AttachedPanel>>,
  #[cfg(test)]
  pub test_attached_hover: Option<bool>,
}

impl Chrome {
  fn update(&mut self, ctx: &egui::Context, window: Rect, hovered: Option<Pos2>) {
    let (now, fixed, pointer_down) = ctx.input(|input| {
      let viewport = input.viewport();
      let fixed =
        [viewport.maximized, viewport.fullscreen, viewport.minimized].contains(&Some(true));
      (input.time, fixed, input.pointer.any_down())
    });
    let restored = std::mem::replace(&mut self.fixed, fixed) && !fixed;
    if let Some(resize) = &self.resize {
      let accepted = (window.height() - resize.target).abs() <= 1.0;
      if !accepted && !fixed && now < resize.deadline {
        ctx.request_repaint_after(Duration::from_secs_f64(resize.deadline - now));
        return;
      }
      self.shown = resize.shown;
      if !fixed {
        // A compositor can clamp or reject a request. Collapse only the
        // height actually added, even when the window could grow partway.
        self.allocated = (self.allocated + window.height() - resize.before).clamp(0.0, BAR_HEIGHT);
      }
      self.resize = None;
      // The window may have moved underneath the pointer during the resize.
      ctx.request_repaint();
      return;
    }

    let reach = if self.shown { BAR_HEIGHT } else { 0.0 } + REVEAL_HEIGHT;
    let hover_zone = Rect::from_min_size(window.min, egui::vec2(window.width(), reach));
    // Leave the canvas in place while an existing gesture crosses the edge.
    let show = hovered.is_some_and(|pos| {
      if pointer_down {
        self.shown
      } else {
        hover_zone.contains(pos)
      }
    });
    if show == self.shown && !restored {
      return;
    }
    let extra = if show { BAR_HEIGHT } else { 0.0 };
    let height = (window.height() + extra - self.allocated).max(super::view::MIN_WINDOW.y);
    if fixed || (height - window.height()).abs() <= 1.0 {
      self.shown = show;
      return;
    }
    self.resize = Some(Resize {
      shown: show,
      before: window.height(),
      target: height,
      deadline: now + RESIZE_WAIT.as_secs_f64(),
    });
    ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(
      window.width(),
      height,
    )));
    ctx.request_repaint_after(RESIZE_WAIT);
  }

  pub fn show(&mut self, ui: &mut egui::Ui, status: Status<'_>) -> Frame {
    let ctx = ui.ctx().clone();
    #[cfg(test)]
    if let Some(hovered) = self.test_attached_hover {
      let action = hovered
        .then(|| show_controls(&ctx, status, true, Some(ctx.content_rect().width())))
        .flatten();
      return Frame::attached().apply(&ctx, action);
    }
    #[cfg(target_os = "macos")]
    {
      if !self.shown
        && self.resize.is_none()
        && self.allocated <= 1.0
        && let Some(frame) = self.show_macos(&ctx, status)
      {
        return frame;
      }
      self.dismiss();
    }
    let window = ctx.content_rect();
    let hovered = ctx.input(|input| input.pointer.hover_pos());
    self.update(&ctx, window, hovered);
    let reveal = Rect::from_min_size(window.min, egui::vec2(window.width(), REVEAL_HEIGHT));
    let bar = Rect::from_min_size(window.min, egui::vec2(window.width(), BAR_HEIGHT));
    let action = self
      .shown
      .then(|| title_bar(ui, bar, status, maximized(&ctx), true))
      .flatten();

    Frame {
      pointer_regions: [
        if self.shown { bar } else { reveal },
        resize_grip(&ctx, window, hovered),
      ],
      close: false,
      toggle_debug: false,
      resizing: self.resize.is_some(),
    }
    .apply(&ctx, action)
  }

  pub fn dismiss(&mut self) {
    #[cfg(target_os = "macos")]
    self.attached.take();
  }

  #[cfg(target_os = "macos")]
  fn show_macos(&mut self, ctx: &egui::Context, status: Status<'_>) -> Option<Frame> {
    let main = MainThreadMarker::new()?;
    let viewer = super::macos_chrome::viewer()?;
    let height = BAR_HEIGHT * ctx.zoom_factor();
    let reveal = REVEAL_HEIGHT * ctx.zoom_factor();
    if !viewer.active() {
      self.dismiss();
      return Some(Frame::attached());
    }
    let initial_width = if let Some(panel) = &self.attached {
      let panel = panel.get(main);
      // AppKit moves the child with its parent. Keep the same attachment even
      // when the pointer leaves the strip or the viewer reaches a screen edge.
      if !panel.dragging() {
        if !panel.hovering(reveal) {
          self.dismiss();
          return Some(Frame::attached());
        }
        if !panel.place(height) {
          return None;
        }
      }
      None
    } else {
      let width = viewer.panel_width(height)?;
      let window = ctx.content_rect();
      let top = Rect::from_min_size(window.min, egui::vec2(window.width(), REVEAL_HEIGHT));
      let enter = ctx.input(|input| {
        !input.pointer.any_down()
          && input
            .pointer
            .hover_pos()
            .is_some_and(|pos| top.contains(pos))
      });
      if !enter {
        return Some(Frame::attached());
      }
      Some(width as f32 / ctx.zoom_factor())
    };

    let action = show_controls(ctx, status, false, initial_width);
    if self.attached.is_none() {
      let panel = viewer.attach_controls(height, status.button_strip_width(), ctx)?;
      self.attached = Some(MainThreadBound::new(panel, main));
    }
    let frame = Frame::attached().apply(ctx, action);
    if frame.close || matches!(action, Some(Action::Minimize)) {
      self.dismiss();
    } else {
      if action.is_some()
        && let Some(panel) = &self.attached
      {
        panel.get(main).restore_focus();
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
fn show_controls(
  ctx: &egui::Context,
  status: Status<'_>,
  draggable: bool,
  initial_width: Option<f32>,
) -> Option<Action> {
  let maximized = maximized(ctx);
  let mut builder = egui::ViewportBuilder::default()
    .with_title(CONTROLS_TITLE)
    .with_decorations(false)
    .with_resizable(false)
    .with_active(false)
    .with_visible(false);
  // AppKit owns placement and size once the panel is attached.
  if let Some(width) = initial_width {
    builder = builder
      .with_clamp_size_to_monitor_size(false)
      .with_inner_size([width, BAR_HEIGHT]);
  }
  ctx.show_viewport_immediate(controls_id(), builder, |panel, class| {
    if class == egui::ViewportClass::EmbeddedWindow {
      return None;
    }
    if panel.input(|input| input.viewport().close_requested()) {
      return Some(Action::Close);
    }
    let bounds = panel.ctx().content_rect();
    title_bar(panel, bounds, status, maximized, draggable)
  })
}

fn title_bar(
  ui: &mut egui::Ui,
  bounds: Rect,
  status: Status<'_>,
  maximized: bool,
  draggable: bool,
) -> Option<Action> {
  let buttons_left = bounds.right() - status.button_strip_width();
  let title = Rect::from_min_max(bounds.min, egui::pos2(buttons_left, bounds.bottom()));
  let dragging = draggable && primary_press_in(ui.ctx(), title);
  egui::Panel::top("ctream_title_bar")
    .exact_size(BAR_HEIGHT)
    .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
    .show_separator_line(false)
    .show(ui, |ui| {
      ui.set_min_size(bounds.size());
      ui.set_clip_rect(bounds);
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
    })
    .inner
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
