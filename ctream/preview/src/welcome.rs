use super::{
  Preview, background, input,
  requests::{Requests, Scoped},
  semantic,
};
use ctream_client::{Client, transport::Transport};
use ctream_proto::{
  CaptureDriverInfo, CaptureScope, Display, Session as DisplaySession, Target, Window,
};
use eframe::egui;
use std::{
  io,
  sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
  },
  time::{Duration, Instant},
};

type Pending<V> = Option<Receiver<io::Result<V>>>;
type Control<T> = ctream_client::input::Input<<T as Transport>::Reader, <T as Transport>::Writer>;

fn receive<V>(pending: &mut Pending<V>) -> Option<io::Result<V>> {
  let result = match pending.as_ref()?.try_recv() {
    Ok(value) => value,
    Err(TryRecvError::Empty) => return None,
    Err(TryRecvError::Disconnected) => Err(io::Error::other("Agent request ended")),
  };
  pending.take();
  Some(result)
}

fn target(session: &DisplaySession) -> Option<Target> {
  match &session.display {
    Display::X11(number) => Some(Target::X11(*number)),
    Display::Wayland { socket, .. } => Target::parse(socket).ok(),
    Display::Windows { number, .. } => Some(Target::Windows(*number)),
  }
}

fn display_label(session: &DisplaySession) -> String {
  match &session.display {
    Display::X11(number) => format!("X11 · :{number}"),
    Display::Wayland { socket, .. } => format!("Wayland · {socket}"),
    Display::Windows { number, bounds, .. } => format!(
      "Display {number} · {} × {}",
      i64::from(bounds[2]) - i64::from(bounds[0]),
      i64::from(bounds[3]) - i64::from(bounds[1])
    ),
  }
}

/// One native window owns setup and viewing. Request generations isolate stale
/// queries, while a successful Start retains exactly the capture being previewed.
pub(super) struct Viewer<T: Transport> {
  transport: Arc<T>,
  displays: Vec<DisplaySession>,
  discovery: Pending<Vec<DisplaySession>>,
  discovery_requests: Option<Requests>,
  discovery_error: Option<String>,
  selected: Option<Target>,
  window_mode: bool,
  selected_window: Option<String>,
  driver: String,
  windows: Vec<Window>,
  window_query: Pending<Vec<Window>>,
  window_error: Option<String>,
  drivers: Vec<CaptureDriverInfo>,
  driver_query: Pending<Vec<CaptureDriverInfo>>,
  driver_error: Option<String>,
  catalog_ready: bool,
  client: Option<Client<Scoped<T>>>,
  requests: Option<Requests>,
  connecting: Pending<Preview<Scoped<T>>>,
  preview: Option<Preview<Scoped<T>>>,
  preview_error: Option<String>,
  preview_after: Instant,
  control: Pending<Control<T>>,
  control_requests: Option<Requests>,
  viewing: bool,
  semantic: Option<semantic::Panel<Scoped<T>>>,
}

impl<T: Transport + Send + Sync + 'static> Viewer<T> {
  pub fn new(
    transport: T,
    selected: Option<Target>,
    window_mode: bool,
    window: Option<String>,
    driver: String,
    ctx: &egui::Context,
  ) -> Self {
    let mut viewer = Self {
      transport: Arc::new(transport),
      displays: Vec::new(),
      discovery: None,
      discovery_requests: None,
      discovery_error: None,
      selected,
      window_mode: window_mode || window.is_some(),
      selected_window: window,
      driver,
      windows: Vec::new(),
      window_query: None,
      window_error: None,
      drivers: Vec::new(),
      driver_query: None,
      driver_error: None,
      catalog_ready: false,
      client: None,
      requests: None,
      connecting: None,
      preview: None,
      preview_error: None,
      preview_after: Instant::now(),
      control: None,
      control_requests: None,
      viewing: false,
      semantic: None,
    };
    viewer.discover(ctx);
    viewer
  }

  fn discard_preview(&mut self) {
    self.control.take();
    self.connecting.take();
    self.semantic = None;
    self.preview = None;
    self.control_requests = None;
    self.preview_error = None;
    self.viewing = false;
    self.preview_after = Instant::now() + Duration::from_millis(180);
  }

  fn clear_source(&mut self) {
    self.discard_preview();
    self.window_query.take();
    self.driver_query.take();
    self.client = None;
    // Active captures close stdin first. This guard also cancels requests still
    // waiting for a catalog reply or the first capture frame.
    self.requests = None;
    self.windows.clear();
    self.drivers.clear();
    self.window_error = None;
    self.driver_error = None;
    self.catalog_ready = false;
  }

  fn discover(&mut self, ctx: &egui::Context) {
    self.clear_source();
    self.displays.clear();
    self.discovery = None;
    self.discovery_requests = None;
    self.discovery_error = None;
    let requests = Requests::new();
    let client = requests.client(Arc::clone(&self.transport));
    self.discovery = Some(background(ctx, move || client.discover()));
    self.discovery_requests = Some(requests);
  }

  fn load_target(&mut self, ctx: &egui::Context) {
    self.clear_source();
    let Some(target) = self.selected else { return };
    let requests = Requests::new();
    let client = requests.client(Arc::clone(&self.transport));
    let query = client.clone();
    self.window_query = Some(background(ctx, move || query.windows(target)));
    let query = client.clone();
    self.driver_query = Some(background(ctx, move || query.capture_drivers(target)));
    self.client = Some(client);
    self.requests = Some(requests);
  }

  fn scope(&self) -> CaptureScope {
    if self.window_mode {
      CaptureScope::Window
    } else {
      CaptureScope::Display
    }
  }

  fn selection_error(&self) -> Option<String> {
    let selected = self.selected?;
    if !self
      .displays
      .iter()
      .any(|session| session.selectable && target(session) == Some(selected))
    {
      return Some("This display is not available to the agent.".into());
    }
    if self.window_mode {
      if let Some(error) = &self.window_error {
        return Some(error.clone());
      }
      if self.window_query.is_some() {
        return Some("Finding windows…".into());
      }
      if !self
        .windows
        .iter()
        .any(|window| Some(&window.id) == self.selected_window.as_ref())
      {
        return Some(
          if self.windows.is_empty() {
            "No capturable windows found."
          } else {
            "Choose a window."
          }
          .into(),
        );
      }
    }
    if !self.catalog_ready {
      return Some("Finding capture drivers…".into());
    }
    if self.driver != "auto" {
      match self.drivers.iter().find(|driver| driver.id == self.driver) {
        Some(driver) if driver.scopes.contains(&self.scope()) => {}
        Some(_) => return Some("This driver does not support the selected source type.".into()),
        None => {
          return Some(
            "This agent does not offer the selected driver. Choose Automatic or another driver."
              .into(),
          );
        }
      }
    }
    None
  }

  fn connect_preview(&mut self, ctx: &egui::Context) {
    if self.discovery.is_some()
      || self.preview.is_some()
      || self.connecting.is_some()
      || self.preview_error.is_some()
      || self.selected.is_none()
      || self.selection_error().is_some()
    {
      return;
    }
    if Instant::now() < self.preview_after {
      ctx.request_repaint_after(self.preview_after.saturating_duration_since(Instant::now()));
      return;
    }
    let Some(client) = &self.client else { return };
    let window = self
      .window_mode
      .then(|| {
        self
          .windows
          .iter()
          .find(|window| Some(&window.id) == self.selected_window.as_ref())
          .cloned()
      })
      .flatten();
    let session = client.session(self.selected.expect("selected display"), window);
    // A catalog failure retains a view-only legacy Auto path. Explicit choices
    // always require the new contract and are never silently substituted.
    let driver = self.driver_error.is_none().then(|| self.driver.clone());
    let render = ctx.clone();
    self.connecting = Some(background(ctx, move || {
      Preview::connect(session, driver.as_deref(), render)
    }));
  }

  fn poll(&mut self, ctx: &egui::Context) {
    if let Some(result) = receive(&mut self.discovery) {
      self.discovery_requests = None;
      match result {
        Ok(displays) => {
          self.displays = displays;
          if self.selected.is_some_and(|selected| {
            !self
              .displays
              .iter()
              .any(|session| session.selectable && target(session) == Some(selected))
          }) {
            self.selected = None;
            self.selected_window = None;
            self.discovery_error =
              Some("The requested display is unavailable. Choose another display.".into());
          }
          if self.selected.is_some() {
            self.load_target(ctx);
          }
        }
        Err(error) => self.discovery_error = Some(format!("Cannot discover displays: {error}")),
      }
    }
    if let Some(result) = receive(&mut self.window_query) {
      match result {
        Ok(windows) => self.windows = windows,
        Err(error) => self.window_error = Some(format!("Cannot list windows: {error}")),
      }
    }
    if let Some(result) = receive(&mut self.driver_query) {
      self.catalog_ready = true;
      match result {
        Ok(drivers) => self.drivers = drivers,
        Err(error) => self.driver_error = Some(error.to_string()),
      }
    }
    if let Some(result) = receive(&mut self.connecting) {
      match result {
        Ok(preview) => self.preview = Some(preview),
        Err(error) => self.preview_error = Some(format!("Cannot open preview: {error}")),
      }
    }
    if let Some(preview) = &mut self.preview
      && let Some(error) = preview.update_texture(ctx)
    {
      self.preview_error = Some(error);
      self.control = None;
      self.semantic = None;
      self.control_requests = None;
      self.requests = None;
    }
    if let Some(result) = receive(&mut self.control)
      && let Some(preview) = &mut self.preview
    {
      match result {
        Ok(control) if self.viewing && preview.ready() => {
          preview.input = Some(input::Input::new(control))
        }
        Ok(_) => {}
        Err(error) => preview.input_error = Some(format!("View only: {error}")),
      }
    }
    if !self.viewing {
      self.connect_preview(ctx);
    }
  }

  fn start(&mut self, ctx: &egui::Context) {
    if self.viewing
      || self.discovery.is_some()
      || self.selection_error().is_some()
      || !self.preview.as_ref().is_some_and(Preview::ready)
    {
      return;
    }
    let Some(target) = self.selected else { return };
    let requests = Requests::new();
    let client = requests.client(Arc::clone(&self.transport));
    let window = self
      .window_mode
      .then(|| {
        self
          .windows
          .iter()
          .find(|window| Some(&window.id) == self.selected_window.as_ref())
          .cloned()
      })
      .flatten();
    let session = client.session(target, window);
    self.viewing = true;
    if session.supports_input() {
      let control = session.clone();
      self.control = Some(background(ctx, move || control.input()));
    }
    if session.supports_semantics() {
      self.semantic = Some(semantic::Panel::new(session));
    }
    self.control_requests = Some(requests);
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1100.0, 720.0)));
  }

  fn back(&mut self, ctx: &egui::Context) {
    self.control = None;
    if let Some(preview) = &mut self.preview {
      preview.stop_control();
    }
    self.semantic = None;
    self.control_requests = None;
    self.viewing = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(860.0, 620.0)));
  }

  fn selectors(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
    let before_display = self.selected;
    let before = (
      self.window_mode,
      self.selected_window.clone(),
      self.driver.clone(),
    );
    ui.label(
      egui::RichText::new("SOURCE")
        .small()
        .strong()
        .color(ui.visuals().weak_text_color()),
    );
    ui.add_space(12.0);
    ui.label("Display");
    let label = self
      .displays
      .iter()
      .find(|session| session.selectable && target(session) == self.selected)
      .map(display_label)
      .unwrap_or_else(|| "Choose a display…".into());
    egui::ComboBox::from_id_salt("display")
      .selected_text(label)
      .truncate()
      .width(ui.available_width())
      .show_ui(ui, |ui| {
        for session in &self.displays {
          if let Some(candidate) = target(session) {
            ui.add_enabled_ui(session.selectable, |ui| {
              ui.selectable_value(&mut self.selected, Some(candidate), display_label(session))
                .on_hover_text(if session.selectable {
                  candidate.to_string()
                } else {
                  "The agent cannot access this session.".into()
                });
            });
          }
        }
      });
    ui.add_space(14.0);
    ui.label("Capture");
    ui.horizontal(|ui| {
      ui.selectable_value(&mut self.window_mode, false, "Display");
      ui.selectable_value(&mut self.window_mode, true, "Window");
    });
    if self.window_mode {
      ui.add_space(10.0);
      ui.label("Window");
      let label = self
        .windows
        .iter()
        .find(|window| Some(&window.id) == self.selected_window.as_ref())
        .map(|window| {
          if window.title.is_empty() {
            window.app_id.clone()
          } else {
            window.title.clone()
          }
        })
        .unwrap_or_else(|| "Choose a window…".into());
      egui::ComboBox::from_id_salt("window")
        .selected_text(label)
        .truncate()
        .width(ui.available_width())
        .show_ui(ui, |ui| {
          for window in &self.windows {
            let label = if window.title.is_empty() {
              &window.app_id
            } else {
              &window.title
            };
            ui.selectable_value(&mut self.selected_window, Some(window.id.clone()), label)
              .on_hover_text(&window.app_id);
          }
        });
    }
    ui.add_space(18.0);
    ui.label("Capture driver");
    let label = if self.driver == "auto" {
      "Automatic".into()
    } else {
      self
        .drivers
        .iter()
        .find(|driver| driver.id == self.driver)
        .map(|driver| driver.name.clone())
        .unwrap_or_else(|| self.driver.clone())
    };
    let scope = self.scope();
    egui::ComboBox::from_id_salt("driver")
      .selected_text(label)
      .truncate()
      .width(ui.available_width())
      .show_ui(ui, |ui| {
        ui.selectable_value(&mut self.driver, "auto".into(), "Automatic");
        for driver in &self.drivers {
          ui.add_enabled_ui(driver.scopes.contains(&scope), |ui| {
            ui.selectable_value(&mut self.driver, driver.id.clone(), &driver.name)
              .on_hover_text(&driver.id);
          });
        }
      });
    if let Some(error) = &self.driver_error {
      ui.add_space(6.0);
      ui.small("Driver list unavailable. Automatic uses the agent’s default.")
        .on_hover_text(error);
    }
    ui.add_space(16.0);
    let refresh = ui
      .add_enabled(
        self.selected.is_some(),
        egui::Button::new("Refresh source list"),
      )
      .clicked();
    let display_changed = before_display != self.selected;
    let changed = before
      != (
        self.window_mode,
        self.selected_window.clone(),
        self.driver.clone(),
      );
    if display_changed {
      self.selected_window = None;
    }
    (display_changed || refresh, changed)
  }

  fn show_setup(&mut self, ctx: &egui::Context) {
    let mut start = false;
    let mut refresh = false;
    let mut reload = false;
    let mut changed = false;
    egui::TopBottomPanel::top("setup_heading").show(ctx, |ui| {
      ui.add_space(6.0);
      ui.horizontal(|ui| {
        ui.heading("ctream");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          refresh = ui.button("Refresh displays").clicked();
        });
      });
      ui.label("Choose a display or window, check the preview, then start.");
      ui.add_space(8.0);
    });
    egui::TopBottomPanel::bottom("setup_actions").show(ctx, |ui| {
      ui.add_space(6.0);
      ui.horizontal(|ui| {
        if self.preview_error.is_some() && ui.button("Retry preview").clicked() {
          reload = true;
        }
        if self.displays.is_empty() && self.discovery.is_none() && self.discovery_error.is_none() {
          ui.label("No displays found.");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          start = ui
            .add_enabled(
              self.preview.as_ref().is_some_and(Preview::ready),
              egui::Button::new("Start").min_size(egui::vec2(110.0, 36.0)),
            )
            .clicked();
        });
      });
      ui.add_space(6.0);
    });
    egui::CentralPanel::default().show(ctx, |ui| {
      if let Some(error) = &self.discovery_error {
        ui.colored_label(ui.visuals().error_fg_color, error);
      }
      if self.discovery.is_some() {
        ui.horizontal(|ui| {
          ui.spinner();
          ui.label("Finding displays…");
        });
      }
      let (body, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
      let selector_width = (body.width() * 0.36).clamp(180.0, 235.0);
      let selectors =
        egui::Rect::from_min_size(body.min, egui::vec2(selector_width, body.height()));
      let pane =
        egui::Rect::from_min_max(egui::pos2(selectors.right() + 22.0, body.top()), body.max);
      let separator_x = selectors.right() + 11.0;
      ui.painter().line_segment(
        [
          egui::pos2(separator_x, body.top()),
          egui::pos2(separator_x, body.bottom()),
        ],
        ui.visuals().widgets.noninteractive.bg_stroke,
      );
      ui.scope_builder(egui::UiBuilder::new().max_rect(selectors), |ui| {
        ui.set_clip_rect(selectors.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
          .id_salt("setup_selectors")
          .auto_shrink([false, false])
          .show(ui, |ui| {
            ui.add_enabled_ui(self.discovery.is_none(), |ui| {
              let selected = self.selectors(ui);
              reload |= selected.0;
              changed |= selected.1;
            });
          });
      });
      ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
        ui.set_clip_rect(pane.intersect(ui.clip_rect()));
        ui.label(
          egui::RichText::new("LIVE PREVIEW")
            .small()
            .strong()
            .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(8.0);
        let preview_height = (ui.available_height() - 68.0).max(80.0);
        let (preview_rect, _) = ui.allocate_exact_size(
          egui::vec2(ui.available_width(), preview_height),
          egui::Sense::hover(),
        );
        ui.painter()
          .rect_filled(preview_rect, 8.0, egui::Color32::from_rgb(14, 17, 23));
        let inner = preview_rect.shrink(12.0);
        {
          // The parent already reserved this rectangle; a child must not move
          // its cursor back inside the pane when it finishes painting.
          let mut preview_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
          let ui = &mut preview_ui;
          ui.set_clip_rect(inner.intersect(ui.clip_rect()));
          if let Some(preview) = &mut self.preview {
            preview.show(ui, self.preview_error.as_deref(), &[], false);
          } else {
            ui.centered_and_justified(|ui| {
              ui.vertical_centered(|ui| {
                if self.connecting.is_some() {
                  ui.spinner();
                }
                ui.label(
                  self
                    .preview_error
                    .clone()
                    .or_else(|| self.selection_error())
                    .unwrap_or_else(|| {
                      if self.connecting.is_some() {
                        "Opening preview…".into()
                      } else {
                        "Select a source to preview".into()
                      }
                    }),
                );
              });
            });
          }
        }
        ui.add_space(8.0);
        ui.add(
          egui::Label::new(
            egui::RichText::new("View only · Mouse and keyboard stay local until Start.").small(),
          )
          .wrap(),
        );
        if let Some(preview) = &self.preview {
          let label = format!(
            "Driver: {}",
            preview.driver.as_deref().unwrap_or("agent default")
          );
          ui.add(egui::Label::new(egui::RichText::new(&label).small()).truncate())
            .on_hover_text(label);
        }
      });
    });
    if refresh {
      self.discover(ctx);
    } else if reload || changed {
      self.load_target(ctx);
    } else if start {
      self.start(ctx);
    }
  }

  fn show_viewer(&mut self, ctx: &egui::Context) {
    let mut back = false;
    egui::TopBottomPanel::top("viewer_navigation").show(ctx, |ui| {
      ui.horizontal(|ui| {
        back = ui.button("Back to setup").clicked();
        ui.separator();
        ui.label(if self.control.is_some() {
          "Enabling control…"
        } else if self
          .preview
          .as_ref()
          .is_some_and(|preview| preview.input.is_some())
        {
          "Control enabled"
        } else {
          "View only"
        });
        if let Some(preview) = &self.preview {
          ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.small(preview.driver.as_deref().unwrap_or("agent default"));
          });
        }
      });
    });
    if back {
      self.back(ctx);
      return;
    }
    let frame_size = self
      .preview
      .as_ref()
      .and_then(|preview| preview.texture.as_ref())
      .map(egui::TextureHandle::size);
    if let Some(semantic) = &mut self.semantic {
      semantic.show(ctx, frame_size);
    }
    egui::CentralPanel::default().show(ctx, |ui| {
      if let Some(preview) = &mut self.preview {
        let elements = self
          .semantic
          .as_ref()
          .map(semantic::Panel::overlay_elements)
          .unwrap_or(&[]);
        preview.show(
          ui,
          self.preview_error.as_deref(),
          elements,
          self.control.is_none(),
        );
      }
    });
  }
}

impl<T: Transport + Send + Sync + 'static> eframe::App for Viewer<T> {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    self.poll(ctx);
    if self.viewing {
      self.show_viewer(ctx);
    } else {
      self.show_setup(ctx);
    }
  }
}

impl<T: Transport> Drop for Viewer<T> {
  fn drop(&mut self) {
    // Close active stdin streams before cancelling any outstanding requests.
    self.preview = None;
    self.semantic = None;
    self.control = None;
    self.control_requests = None;
    self.connecting = None;
    self.requests = None;
    self.discovery = None;
    self.discovery_requests = None;
  }
}

#[cfg(test)]
#[path = "welcome_tests.rs"]
mod tests;
