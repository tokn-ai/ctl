use super::view::viewer_id;
use super::{
  CaptureEnd, Preview, PreviewMode, background, chrome, drivers, input, latency,
  requests::{Requests, Scoped, Traffic},
  semantic, setup,
  stats::Stats,
  view,
};
use ctream_client::{Client, transport::Transport};
use ctream_proto::{
  CaptureDriverDescriptor, CaptureScope, CaptureSystem, Display, Session as DisplaySession, Target,
  Window,
};
use eframe::egui;
use std::{
  collections::{HashMap, hash_map::Entry},
  io,
  sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
  },
  time::{Duration, Instant},
};

type Pending<V> = Option<Receiver<io::Result<V>>>;
type Control<T> =
  ctream_client::input::Input<<Scoped<T> as Transport>::Reader, <Scoped<T> as Transport>::Writer>;

struct Catalog {
  drivers: Vec<CaptureDriverDescriptor>,
  // None is an agent-wide catalog. Older agents can only advertise providers
  // for targets we have discovered; other targets retain legacy Automatic.
  targets: Option<Vec<Target>>,
}

fn legacy_catalog<T: Transport>(client: &Client<T>, targets: Vec<Target>) -> Catalog {
  let mut drivers: Vec<CaptureDriverDescriptor> = Vec::new();
  let mut supported = Vec::new();
  for target in targets {
    if let Ok(entries) = client.capture_drivers(target) {
      supported.push(target);
      for info in entries {
        if let Some(existing) = drivers.iter_mut().find(|driver| driver.info.id == info.id) {
          if !existing.systems.contains(&target.system()) {
            existing.systems.push(target.system());
          }
          for scope in info.scopes {
            if !existing.info.scopes.contains(&scope) {
              existing.info.scopes.push(scope);
            }
          }
        } else {
          drivers.push(CaptureDriverDescriptor {
            info,
            systems: vec![target.system()],
          });
        }
      }
    }
  }
  Catalog {
    drivers,
    targets: Some(supported),
  }
}

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
    Display::MacosDesktop => Some(Target::MacosDesktop),
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
    Display::MacosDesktop => "macOS desktop".into(),
  }
}

struct WindowSource {
  target: Target,
  window: Window,
}

fn window_sources<T: Transport>(
  client: &Client<T>,
  targets: Vec<Target>,
  windows_desktop: bool,
) -> io::Result<Vec<WindowSource>> {
  let mut sources = Vec::new();
  let mut desktop_error = None;
  let mut error = None;
  let mut listed = false;
  let mut desktop_listed = false;
  let mut legacy_windows = HashMap::new();
  if windows_desktop {
    match client.windows(Target::WindowsDesktop) {
      Ok(windows) => {
        desktop_listed = true;
        listed = true;
        sources.extend(windows.into_iter().map(|window| WindowSource {
          target: Target::WindowsDesktop,
          window,
        }));
      }
      Err(err) => desktop_error = Some(err),
    }
  }
  for target in targets {
    if desktop_listed && matches!(target, Target::Windows(_)) {
      continue;
    }
    match client.windows(target) {
      Ok(windows) => {
        listed = true;
        for window in windows {
          // A legacy Windows window can move between monitor queries. Its
          // identity is desktop-wide even when the old request needs a monitor.
          if matches!(target, Target::Windows(_)) {
            match legacy_windows.entry(window.id.clone()) {
              Entry::Occupied(entry) => {
                sources[*entry.get()] = WindowSource { target, window };
                continue;
              }
              Entry::Vacant(entry) => {
                entry.insert(sources.len());
              }
            }
          }
          sources.push(WindowSource { target, window });
        }
      }
      Err(err) => {
        error.get_or_insert(err);
      }
    }
  }
  if let Some(error) = error.or(desktop_error).filter(|_| !listed) {
    return Err(error);
  }
  Ok(sources)
}

/// Setup owns the capture and a separate native viewer temporarily owns control.
/// Request generations isolate stale queries; Start retains the preview stream.
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
  windows: Vec<WindowSource>,
  window_query: Pending<Vec<WindowSource>>,
  window_error: Option<String>,
  source_loaded: bool,
  drivers: Vec<CaptureDriverDescriptor>,
  driver_query: Pending<Catalog>,
  catalog_requests: Option<Requests>,
  catalog_targets: Option<Vec<Target>>,
  catalog_fallback: bool,
  driver_error: Option<String>,
  catalog_ready: bool,
  client: Option<Client<Scoped<T>>>,
  requests: Option<Requests>,
  traffic: Traffic,
  connecting: Pending<Preview<Scoped<T>>>,
  preview: Option<Preview<Scoped<T>>>,
  preview_error: Option<String>,
  preview_after: Instant,
  control: Pending<Control<T>>,
  control_requests: Option<Requests>,
  viewing: bool,
  viewer_opening: bool,
  viewer_size: view::AspectResize,
  chrome: chrome::Chrome,
  semantic: Option<semantic::Panel<Scoped<T>>>,
  show_stats: bool,
  stats: Option<Stats>,
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
    let window_mode = window_mode || window.is_some() || selected.is_some_and(Target::window_only);
    let mut viewer = Self {
      transport: Arc::new(transport),
      displays: Vec::new(),
      discovery: None,
      discovery_requests: None,
      discovery_error: None,
      selected,
      window_mode,
      selected_window: window,
      driver,
      windows: Vec::new(),
      window_query: None,
      window_error: None,
      source_loaded: false,
      drivers: Vec::new(),
      driver_query: None,
      catalog_requests: None,
      catalog_targets: Some(Vec::new()),
      catalog_fallback: false,
      driver_error: None,
      catalog_ready: false,
      client: None,
      requests: None,
      traffic: Traffic::default(),
      connecting: None,
      preview: None,
      preview_error: None,
      preview_after: Instant::now(),
      control: None,
      control_requests: None,
      viewing: false,
      viewer_opening: false,
      viewer_size: view::AspectResize::default(),
      chrome: chrome::Chrome::default(),
      semantic: None,
      show_stats: false,
      stats: None,
    };
    viewer.discover(ctx);
    viewer
  }

  fn discard_preview(&mut self) {
    self.stats = None;
    self.control.take();
    self.connecting.take();
    self.semantic = None;
    self.preview = None;
    self.control_requests = None;
    self.preview_error = None;
    self.viewing = false;
    self.viewer_opening = false;
    self.preview_after = Instant::now() + Duration::from_millis(180);
  }

  fn clear_source(&mut self) {
    self.reload_preview();
    self.window_query.take();
    self.windows.clear();
    self.window_error = None;
    self.source_loaded = false;
  }

  fn reload_preview(&mut self) {
    self.discard_preview();
    self.client = None;
    // Close active captures before cancelling their transport scope.
    // A pending window query shares this scope until its results arrive.
    self.requests = None;
    self.traffic = Traffic::default();
  }

  fn discover(&mut self, ctx: &egui::Context) {
    if self.viewing {
      return;
    }
    self.clear_source();
    self.displays.clear();
    self.discovery = None;
    self.discovery_requests = None;
    self.discovery_error = None;
    self.driver_query = None;
    self.catalog_requests = None;
    self.drivers.clear();
    self.driver_error = None;
    self.catalog_ready = false;
    self.catalog_fallback = false;
    self.catalog_targets = Some(Vec::new());
    let requests = Requests::new();
    let client = requests.client(Arc::clone(&self.transport));
    self.discovery = Some(background(ctx, move || client.discover()));
    self.discovery_requests = Some(requests);
    let requests = Requests::new();
    let client = requests.client(Arc::clone(&self.transport));
    self.driver_query = Some(background(ctx, move || {
      client.capture_catalog().map(|drivers| Catalog {
        drivers,
        targets: None,
      })
    }));
    self.catalog_requests = Some(requests);
  }

  fn load_target(&mut self, ctx: &egui::Context) {
    if self.viewing {
      return;
    }
    self.clear_source();
    if self.discovery.is_some() || !self.catalog_ready {
      return;
    }
    self.source_loaded = true;
    if !self.available_scopes().contains(&self.scope()) {
      return;
    }
    if self.window_mode {
      let targets: Vec<_> = self.available_targets().collect();
      let windows_desktop = self.has_windows_desktop(&targets);
      if !targets.is_empty() || windows_desktop {
        self.make_client();
        let query = self.client.as_ref().unwrap().clone();
        self.window_query = Some(background(ctx, move || {
          window_sources(&query, targets, windows_desktop)
        }));
      }
    } else if self.selected.is_none() {
      let first = self.available_targets().next();
      self.selected = first;
    }
  }

  fn make_client(&mut self) {
    let requests = Requests::metered(self.traffic.clone());
    let client = requests.client(Arc::clone(&self.transport));
    self.client = Some(client);
    self.requests = Some(requests);
  }

  fn available_targets(&self) -> impl Iterator<Item = Target> + '_ {
    self
      .displays
      .iter()
      .filter(|session| session.selectable)
      .filter_map(target)
      .filter(|target| self.window_mode || !target.window_only())
      .filter(|target| self.compatible(*target))
  }

  fn has_windows_desktop(&self, targets: &[Target]) -> bool {
    targets
      .iter()
      .any(|target| matches!(target, Target::Windows(_)))
      || self.drivers.iter().any(|driver| {
        (self.driver == "auto" || self.driver == driver.info.id)
          && driver.systems.contains(&CaptureSystem::Windows)
          && driver.info.scopes.contains(&CaptureScope::Window)
      })
  }

  fn use_window_sources(&mut self, windows: Vec<WindowSource>) {
    let preferred = |source: &&WindowSource| {
      self.selected.is_none_or(|target| {
        source.target == target
          || (matches!(target, Target::Windows(_)) && source.target == Target::WindowsDesktop)
      })
    };
    let selection = if let Some(id) = self.selected_window.as_deref() {
      windows.iter().find(|source| {
        source.window.id == id
          && (preferred(source)
            || self.selected.is_some_and(|target| {
              target.system() == CaptureSystem::Windows
                && source.target.system() == CaptureSystem::Windows
            }))
      })
    } else {
      windows.iter().find(preferred).or_else(|| windows.first())
    };
    if let Some(source) = selection {
      self.selected = Some(source.target);
      self.selected_window = Some(source.window.id.clone());
    }
    self.windows = windows;
  }

  fn selected_window(&self) -> Option<&Window> {
    self.windows.iter().find_map(|source| {
      (Some(source.target) == self.selected
        && Some(&source.window.id) == self.selected_window.as_ref())
      .then_some(&source.window)
    })
  }

  fn scope(&self) -> CaptureScope {
    if self.window_mode {
      CaptureScope::Window
    } else {
      CaptureScope::Display
    }
  }

  fn compatible(&self, target: Target) -> bool {
    self.driver == "auto"
      || self
        .drivers
        .iter()
        .any(|driver| driver.info.id == self.driver && driver.systems.contains(&target.system()))
  }

  fn choose_driver(&mut self, id: &str) {
    if self.viewing {
      return;
    }
    let changed = self.driver != id;
    let old_scope = self.scope();
    self.driver = id.into();
    self.apply_driver_scope();
    if changed || old_scope != self.scope() {
      self.selected = None;
      self.selected_window = None;
    }
  }

  fn apply_driver_scope(&mut self) {
    match self.available_scopes().as_slice() {
      [CaptureScope::Display] => self.window_mode = false,
      [CaptureScope::Window] => self.window_mode = true,
      _ => {}
    }
  }

  fn available_scopes(&self) -> Vec<CaptureScope> {
    if self.driver == "auto" {
      // macOS hosts offer a window catalog in place of monitor sources.
      if !self.drivers.is_empty()
        && self.drivers.iter().all(|driver| {
          driver.systems.as_slice() == [CaptureSystem::Macos]
            && driver.info.scopes.as_slice() == [CaptureScope::Window]
        })
      {
        return vec![CaptureScope::Window];
      }
      return vec![CaptureScope::Display, CaptureScope::Window];
    }
    self
      .drivers
      .iter()
      .find(|driver| driver.info.id == self.driver)
      .map(|driver| driver.info.scopes.clone())
      .unwrap_or_default()
  }

  fn selected_catalog_supported(&self) -> bool {
    self.catalog_targets.as_ref().is_none_or(|targets| {
      self
        .selected
        .is_some_and(|target| targets.contains(&target))
    })
  }

  fn selection_error(&self) -> Option<String> {
    if !self.catalog_ready {
      return Some("Finding capture drivers…".into());
    }
    if self.driver != "auto" {
      match self.drivers.iter().find(|driver| driver.info.id == self.driver) {
        Some(driver) if driver.info.scopes.contains(&self.scope()) => {}
        Some(_) => return Some("This driver does not support the requested source type. Select a driver card to choose a compatible source.".into()),
        None => return Some("This agent does not offer the selected driver. Choose another driver.".into()),
      }
    }
    if self.window_mode {
      if let Some(error) = &self.window_error {
        return Some(error.clone());
      }
      if self.window_query.is_some() {
        return Some("Finding windows…".into());
      }
      if self.selected_window().is_none() {
        return Some(
          if self.selected_window.is_some() {
            "The requested window is unavailable. Choose another window."
          } else if self.windows.is_empty() {
            if self.available_targets().next().is_none() && !self.has_windows_desktop(&[]) {
              "No compatible sources are available for this driver."
            } else {
              "No capturable windows found."
            }
          } else {
            "Choose a window."
          }
          .into(),
        );
      }
    }
    let Some(selected) = self.selected else {
      return Some(
        if self.available_targets().next().is_some() {
          "Choose a source for this driver."
        } else {
          "No compatible sources are available for this driver."
        }
        .into(),
      );
    };
    if !self.compatible(selected) {
      return Some("Choose a source supported by this driver.".into());
    }
    if self.driver != "auto" && !self.selected_catalog_supported() {
      return Some("This source cannot select an explicit driver with the current agent.".into());
    }
    if !self.window_mode
      && !self
        .displays
        .iter()
        .any(|session| session.selectable && target(session) == Some(selected))
    {
      return Some("This display is not available to the agent.".into());
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
    if self.client.is_none() {
      self.make_client();
    }
    let client = self.client.as_ref().unwrap();
    let window = self
      .window_mode
      .then(|| self.selected_window().cloned())
      .flatten();
    let session = client.session(self.selected.expect("selected display"), window);
    // A catalog failure retains a legacy Auto path. Explicit choices
    // always require the new contract and are never silently substituted.
    let driver = self
      .selected_catalog_supported()
      .then(|| self.driver.clone());
    let render = ctx.clone();
    self.connecting = Some(background(ctx, move || {
      Preview::connect(session, driver.as_deref(), render)
    }));
  }

  fn poll(&mut self, ctx: &egui::Context) {
    if let Some(result) = receive(&mut self.discovery) {
      self.discovery_requests = None;
      match result {
        Ok(displays) => self.displays = displays,
        Err(error) => self.discovery_error = Some(format!("Cannot discover displays: {error}")),
      }
    }
    if let Some(result) = receive(&mut self.window_query) {
      match result {
        Ok(windows) => self.use_window_sources(windows),
        Err(error) => self.window_error = Some(format!("Cannot list windows: {error}")),
      }
    }
    if let Some(result) = receive(&mut self.driver_query) {
      match result {
        Ok(catalog) => {
          self.catalog_requests = None;
          self.catalog_ready = true;
          self.drivers = catalog.drivers;
          self.catalog_targets = catalog.targets;
          if !self.drivers.is_empty() {
            self.driver_error = None;
          }
          // Metadata may narrow an unqualified source to a window. It must not
          // widen a requested window into whole-display capture: only an
          // explicit card click can change that intent.
          if !self.window_mode
            || self.drivers.iter().any(|driver| {
              driver.info.id == self.driver && driver.info.scopes.contains(&CaptureScope::Window)
            })
          {
            self.apply_driver_scope();
          }
        }
        Err(error) => {
          self.catalog_requests = None;
          self.driver_error = Some(error.to_string());
          // A failed legacy query is terminal. An unsupported new catalog gets
          // one fallback once display discovery has supplied the old targets.
          self.catalog_ready = self.catalog_fallback;
        }
      }
    }
    if !self.catalog_ready && self.driver_query.is_none() && self.discovery.is_none() {
      self.catalog_fallback = true;
      let targets = self.displays.iter().filter_map(target).collect();
      let requests = Requests::new();
      let client = requests.client(Arc::clone(&self.transport));
      self.driver_query = Some(background(ctx, move || {
        Ok(legacy_catalog(&client, targets))
      }));
      self.catalog_requests = Some(requests);
    }
    if let Some(result) = receive(&mut self.connecting) {
      match result {
        Ok(preview) => self.preview = Some(preview),
        Err(error) => self.preview_error = Some(format!("Cannot open preview: {error}")),
      }
    }
    if let Some(preview) = &mut self.preview
      && let Some(end) = preview.update_texture(ctx)
    {
      if self.viewing && self.window_mode && matches!(end, CaptureEnd::Finished) {
        self.close_viewer(ctx);
        self.load_target(ctx);
      } else {
        self.preview_error = Some(match end {
          CaptureEnd::Finished => "Capture ended.".into(),
          CaptureEnd::Failed(error) => error,
        });
        self.control = None;
        self.semantic = None;
        self.control_requests = None;
        self.requests = None;
      }
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
      if !self.source_loaded && self.catalog_ready && self.discovery.is_none() {
        self.load_target(ctx);
      }
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
    let requests = Requests::metered(self.traffic.clone());
    let client = requests.client(Arc::clone(&self.transport));
    let window = self
      .window_mode
      .then(|| self.selected_window().cloned())
      .flatten();
    let session = client.session(target, window);
    self.viewing = true;
    self.viewer_opening = true;
    self.stats = self.show_stats.then(|| {
      Stats::new(
        self.traffic.clone(),
        latency::Latency::start(Arc::clone(&self.transport), ctx),
      )
    });
    if session.supports_input() {
      let control = session.clone();
      self.control = Some(background(ctx, move || control.input()));
    }
    if session.supports_semantics() {
      self.semantic = Some(semantic::Panel::new(session));
    }
    self.control_requests = Some(requests);
    ctx.request_repaint_of(egui::ViewportId::ROOT);
  }

  fn close_viewer(&mut self, ctx: &egui::Context) {
    self.chrome.dismiss();
    self.stats = None;
    self.control = None;
    if let Some(preview) = &mut self.preview {
      preview.stop_control();
    }
    self.semantic = None;
    self.control_requests = None;
    self.viewing = false;
    self.viewer_opening = false;
    view::restore_setup(ctx);
  }

  fn selectors(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
    let before_source = (self.selected, self.selected_window.clone());
    let before_driver = self.driver.clone();
    let before_scope = self.scope();
    setup::heading(ui, "CAPTURE DRIVER");
    if let Some(id) = drivers::chooser(ui, &self.drivers, &self.driver, &self.displays) {
      self.choose_driver(&id);
    }
    if !self.catalog_ready {
      ui.horizontal(|ui| {
        ui.spinner();
        ui.label("Finding capture drivers…");
      });
    } else if let Some(error) = &self.driver_error {
      ui.small("Driver catalog unavailable. Automatic uses the agent’s default.")
        .on_hover_text(error);
    }
    ui.add_space(18.0);
    setup::heading(ui, "SOURCE");
    // Scope is a property of a provider. Only a provider supporting both needs
    // a second choice; selecting a single-scope provider sets it directly.
    let scopes = self.available_scopes();
    if scopes.contains(&CaptureScope::Display) && scopes.contains(&CaptureScope::Window) {
      let previous = self.window_mode;
      ui.horizontal(|ui| {
        ui.selectable_value(&mut self.window_mode, false, "Display");
        ui.selectable_value(&mut self.window_mode, true, "Window");
      });
      if previous != self.window_mode {
        self.selected = None;
        self.selected_window = None;
      }
      ui.add_space(8.0);
    }
    if self.window_mode {
      self.window_picker(ui);
    } else {
      self.display_picker(ui);
    }
    let refresh = self.window_mode && {
      ui.add_space(16.0);
      ui.add_enabled(
        self.discovery.is_none() && self.catalog_ready,
        egui::Button::new("Refresh windows"),
      )
      .clicked()
    };
    (
      refresh || before_driver != self.driver || before_scope != self.scope(),
      before_source != (self.selected, self.selected_window.clone()),
    )
  }

  fn display_picker(&mut self, ui: &mut egui::Ui) {
    ui.label("Display / session");
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
          if let Some(candidate) = target(session)
            .filter(|candidate| !candidate.window_only() && self.compatible(*candidate))
          {
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
  }

  fn window_picker(&mut self, ui: &mut egui::Ui) {
    let label = self
      .selected_window()
      .map(|window| setup::window_name(&window.title, &window.app_id));
    let choices = self.windows.iter().map(|source| {
      let window = &source.window;
      (
        (source.target, &window.id),
        self.selected == Some(source.target) && self.selected_window.as_ref() == Some(&window.id),
        window.title.as_str(),
        window.app_id.as_str(),
      )
    });
    let choice = setup::window_picker(ui, label, choices);
    if let Some((target, id)) = choice {
      self.selected = Some(target);
      self.selected_window = Some(id.clone());
    }
  }

  fn show_setup(&mut self, ctx: &egui::Context) {
    if self.viewing {
      return;
    }
    let mut start = false;
    let mut refresh = false;
    let mut reload = false;
    let mut changed = false;
    egui::TopBottomPanel::top("setup_heading").show(ctx, |ui| {
      ui.add_space(6.0);
      ui.horizontal(|ui| {
        ui.heading("ctream");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
          refresh = ui.button("Refresh drivers and sources").clicked();
        });
      });
      ui.label("Choose a capture driver, select its source, then start.");
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
          ui.checkbox(&mut self.show_stats, "Debug overlay");
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
      let (selectors, pane) = setup::panes(ui);
      ui.scope_builder(egui::UiBuilder::new().max_rect(selectors), |ui| {
        ui.set_clip_rect(selectors.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
          .id_salt("setup_selectors")
          .auto_shrink([false, false])
          .show(ui, |ui| {
            let selected = self.selectors(ui);
            reload |= selected.0;
            changed |= selected.1;
          });
      });
      ui.scope_builder(egui::UiBuilder::new().max_rect(pane), |ui| {
        ui.set_clip_rect(pane.intersect(ui.clip_rect()));
        setup::heading(ui, "LIVE PREVIEW");
        let preview_height = (ui.available_height() - 68.0).max(80.0);
        let (preview_rect, _) = ui.allocate_exact_size(
          egui::vec2(ui.available_width(), preview_height),
          egui::Sense::hover(),
        );
        let inner = if self
          .preview
          .as_ref()
          .is_some_and(|preview| preview.texture.is_some())
        {
          preview_rect
        } else {
          ui.painter()
            .rect_filled(preview_rect, 8.0, egui::Color32::from_rgb(14, 17, 23));
          preview_rect.shrink(12.0)
        };
        {
          // The parent already reserved this rectangle; a child must not move
          // its cursor back inside the pane when it finishes painting.
          let mut preview_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));
          let ui = &mut preview_ui;
          ui.set_clip_rect(inner.intersect(ui.clip_rect()));
          if let Some(preview) = &mut self.preview {
            let _ = preview.show(ui, self.preview_error.as_deref(), &[], PreviewMode::Setup);
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
            egui::RichText::new("View only · Start opens a separate viewer with control.").small(),
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
    } else if reload {
      self.load_target(ctx);
    } else if changed {
      self.reload_preview();
    } else if start {
      self.start(ctx);
    }
  }

  fn show_viewer(&mut self, ctx: &egui::Context) {
    let preview = self.preview.as_ref();
    let error = self
      .preview_error
      .as_deref()
      .or_else(|| preview.and_then(|preview| preview.input_error.as_deref()));
    let status = if self.control.is_some() {
      "Enabling control…"
    } else if preview.is_some_and(|preview| preview.input.is_some()) {
      "Control enabled"
    } else {
      "View only"
    };
    let chrome = self.chrome.show(
      ctx,
      chrome::Status {
        text: error.unwrap_or(status),
        error: error.is_some(),
        driver: preview.map(|preview| preview.driver.as_deref().unwrap_or("agent default")),
      },
    );
    if chrome.close {
      self.close_viewer(ctx);
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
    let video = ctx.available_rect().size();
    egui::CentralPanel::default()
      .frame(egui::Frame::NONE)
      .show(ctx, |ui| {
        let Some(preview) = &mut self.preview else {
          return;
        };
        let elements = self
          .semantic
          .as_ref()
          .map(semantic::Panel::overlay_elements)
          .unwrap_or(&[]);
        let image = preview.show(
          ui,
          self.preview_error.as_deref(),
          elements,
          PreviewMode::Viewer {
            interactive: self.control.is_none(),
            pointer_regions: &chrome.pointer_regions,
          },
        );
        if let (Some(image), Some(frame_size), Some(stats)) = (image, frame_size, &mut self.stats) {
          stats.paint(ui.painter(), image, preview.frames, frame_size);
        }
        if !chrome.resizing
          && let Some(size) = preview.image_size()
        {
          self.viewer_size.update(ctx, size, video);
        }
      });
  }

  fn show_viewer_window(&mut self, ctx: &egui::Context) {
    let initial = self.viewer_opening.then(|| {
      let image = self
        .preview
        .as_ref()
        .and_then(Preview::image_size)
        .unwrap_or([16, 9]);
      let sidebar = if self.semantic.is_some() {
        semantic::PANEL_WIDTH
      } else {
        0.0
      };
      self.viewer_size = view::AspectResize::default();
      self.chrome = chrome::Chrome::default();
      (image, sidebar)
    });
    let builder = view::viewer_builder(ctx, initial);
    ctx.show_viewport_immediate(viewer_id(), builder, |child, class| {
      if class == egui::ViewportClass::Embedded {
        self.close_viewer(child);
        self.preview_error =
          Some("This backend cannot open a separate native viewer window.".into());
        return;
      }
      if child.input(|input| input.viewport().close_requested()) {
        self.close_viewer(child);
        return;
      }
      if self.viewer_opening {
        // Wait until the native child exists before hiding its parent. Scope
        // every command explicitly so a child event cannot close the app.
        view::hide_setup(child);
        self.viewer_opening = false;
      }
      self.show_viewer(child);
    });
  }

  pub(super) fn is_viewing(&self) -> bool {
    self.viewing
  }

  pub(super) fn update_ui(&mut self, ctx: &egui::Context) {
    if ctx.input(|input| input.viewport().close_requested()) {
      self.shutdown();
      return;
    }
    self.poll(ctx);
    let setup_shown = !self.viewing;
    if setup_shown {
      self.show_setup(ctx);
    }
    if self.viewing {
      self.show_viewer_window(ctx);
    }
    if !setup_shown && !self.viewing {
      // Paint the retained setup in the same pass that makes ROOT visible,
      // rather than briefly restoring an empty hidden-window frame.
      self.show_setup(ctx);
    }
  }
}

impl<T: Transport + Send + Sync + 'static> eframe::App for Viewer<T> {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    self.update_ui(ctx);
  }
}

impl<T: Transport> Viewer<T> {
  fn shutdown(&mut self) {
    self.chrome.dismiss();
    self.stats = None;
    // Close active stdin streams before cancelling any outstanding requests.
    self.preview = None;
    self.semantic = None;
    self.control = None;
    self.control_requests = None;
    self.connecting = None;
    self.requests = None;
    self.discovery = None;
    self.discovery_requests = None;
    self.driver_query = None;
    self.catalog_requests = None;
    self.viewing = false;
    self.viewer_opening = false;
  }
}

impl<T: Transport> Drop for Viewer<T> {
  fn drop(&mut self) {
    self.shutdown();
  }
}

#[cfg(test)]
#[path = "welcome_tests.rs"]
mod tests;
