use ctream::{
  Display,
  client::{Client, Session},
  media::{CaptureLifetime, Frame, VideoSource},
  protocol::{Element, Session as DisplaySession, Target, Window, X11_FRAME_SIZE},
  transport::{Local, Ssh, Transport},
};
mod input;
mod overlay;
mod semantic;
mod view;

use eframe::egui;
use std::{
  env, io,
  sync::{
    Arc, Mutex,
    mpsc::{self, Receiver, TryRecvError},
  },
  thread,
};

#[derive(Default)]
struct Stream {
  frame: Option<egui::ColorImage>,
  error: Option<String>,
}

struct Preview<T: Transport> {
  input: Option<input::Input<T::Reader, T::Writer>>,
  forwarding: bool,
  input_error: Option<String>,
  _capture_lifetime: CaptureLifetime<T::Writer>,
  stream: Arc<Mutex<Stream>>,
  texture: Option<egui::TextureHandle>,
  frames: u64,
}

impl<T: Transport> Preview<T> {
  fn connect(session: Session<T>, ctx: egui::Context) -> io::Result<Self> {
    let (input, input_error) = if session.supports_input() {
      match session.input() {
        Ok(input) => (Some(input::Input::new(input)), None),
        Err(error) => (None, Some(format!("View only ({error})"))),
      }
    } else {
      (None, None)
    };
    let (mut source, lifetime) = session.capture()?.split();
    let stream = Arc::new(Mutex::new(Stream::default()));
    let reader = Arc::clone(&stream);
    thread::spawn(move || {
      let mut frame = Frame::default();
      loop {
        if let Err(error) = source.read_frame(&mut frame) {
          reader.lock().unwrap().error = Some(format!(
            "Capture ended ({error}). See terminal for remote diagnostics."
          ));
          ctx.request_repaint();
          break;
        }
        let image = egui::ColorImage::from_rgb(frame.size, &frame.pixels);
        reader.lock().unwrap().frame = Some(image);
        ctx.request_repaint();
      }
    });
    Ok(Self {
      input,
      forwarding: false,
      input_error,
      _capture_lifetime: lifetime,
      stream,
      texture: None,
      frames: 0,
    })
  }

  fn update_texture(&mut self, ctx: &egui::Context) -> Option<String> {
    let (frame, error) = {
      let mut stream = self.stream.lock().unwrap();
      (stream.frame.take(), stream.error.clone())
    };
    if let Some(image) = frame {
      if let Some(texture) = &mut self.texture {
        texture.set(image, egui::TextureOptions::LINEAR);
      } else {
        self.texture = Some(ctx.load_texture("desktop", image, egui::TextureOptions::LINEAR));
      }
      self.frames += 1;
      if self.frames == 2 {
        eprintln!("Live preview received two frames on the wgpu renderer.");
      }
    }
    error
  }

  fn show(&mut self, ctx: &egui::Context, error: Option<&str>, elements: &[Element]) {
    egui::CentralPanel::default().show(ctx, |ui| {
      if let Some(error) = error.or(self.input_error.as_deref()) {
        ui.label(error);
      } else if self.texture.is_none() {
        ui.spinner();
        ui.label("Waiting for the first frame…");
      }
      if let Some(texture) = &self.texture {
        let size = self
          .input
          .as_ref()
          .and_then(input::Input::desktop_size)
          .unwrap_or_else(|| texture.size().map(|v| v as u32));
        let Some(image) = view::fit(ui.available_rect_before_wrap(), size) else {
          if self.forwarding
            && let Some(input) = &mut self.input
          {
            self.forwarding = false;
            self.input_error = input
              .reset()
              .err()
              .map(|error| format!("Input disconnected ({error})"));
          }
          return;
        };
        let rect = view::rect(image.viewport());
        ui.painter().image(
          texture.id(),
          rect,
          egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
          egui::Color32::WHITE,
        );
        if error.is_none() {
          overlay::paint(ui.painter(), &image, elements);
        }
        if let Some(input) = &mut self.input
          && ctx.current_pass_index() == 0
          && self.input_error.is_none()
        {
          let active = error.is_none() && ctx.input(|i| i.focused);
          let result = if active {
            ctx.input(|i| input.forward(&image, &i.raw.events, i.modifiers))
          } else if self.forwarding {
            input.reset()
          } else {
            Ok(())
          };
          self.forwarding = active;
          if let Err(err) = result {
            self.input_error = Some(format!("Input disconnected ({err})"));
          }
        }
      }
    });
  }
}

enum State<T: Transport> {
  Discovering(Receiver<io::Result<Vec<DisplaySession>>>),
  Selecting(Vec<DisplaySession>),
  FindingWindows(Target, Receiver<io::Result<Vec<Window>>>),
  ChoosingWindow(Target, Vec<Window>),
  Connecting(Receiver<io::Result<Preview<T>>>),
  Viewing(Preview<T>),
  Failed(String),
}

fn background<T: Send + 'static>(
  ctx: &egui::Context,
  task: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Receiver<io::Result<T>> {
  let ctx = ctx.clone();
  let (sender, receiver) = mpsc::channel();
  thread::spawn(move || {
    let _ = sender.send(task());
    ctx.request_repaint();
  });
  receiver
}

struct Viewer<T: Transport> {
  client: Client<T>,
  state: State<T>,
  semantic: Option<semantic::Panel<T>>,
  window_display: Option<Target>,
}

impl<T: Transport + Send + Sync + 'static> Viewer<T> {
  fn discover(&mut self, ctx: &egui::Context) {
    self.semantic = None;
    self.window_display = None;
    let client = self.client.clone();
    self.state = State::Discovering(background(ctx, move || client.discover()));
  }

  fn windows(&mut self, target: Target, ctx: &egui::Context) {
    let client = self.client.clone();
    self.state = State::FindingWindows(target, background(ctx, move || client.windows(target)));
  }

  fn connect(&mut self, target: Target, window: Option<Window>, ctx: &egui::Context) {
    self.window_display = window.as_ref().map(|_| target);
    let session = self.client.session(target, window);
    self.semantic = session
      .supports_semantics()
      .then(|| semantic::Panel::new(session.clone()));
    let render = ctx.clone();
    self.state = State::Connecting(background(ctx, move || Preview::connect(session, render)));
  }
}

fn receive<T, V: Transport>(
  result: Result<io::Result<T>, TryRecvError>,
  success: impl FnOnce(T) -> State<V>,
) -> Option<State<V>> {
  match result {
    Ok(Ok(value)) => Some(success(value)),
    Ok(Err(error)) => Some(State::Failed(error.to_string())),
    Err(TryRecvError::Empty) => None,
    Err(TryRecvError::Disconnected) => Some(State::Failed("Agent connection task ended".into())),
  }
}

impl<T: Transport + Send + Sync + 'static> eframe::App for Viewer<T> {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    let next = match &self.state {
      State::Discovering(result) => receive(result.try_recv(), State::Selecting),
      State::FindingWindows(target, result) => receive(result.try_recv(), |windows| {
        State::ChoosingWindow(*target, windows)
      }),
      State::Connecting(result) => receive(result.try_recv(), State::Viewing),
      _ => None,
    };
    if let Some(next) = next {
      self.state = next;
    }
    if let State::Viewing(preview) = &mut self.state {
      if let Some(target) = self.window_display {
        let mut back = false;
        egui::TopBottomPanel::top("window_navigation").show(ctx, |ui| {
          back = ui.button("Back to windows").clicked();
        });
        if back {
          self.semantic = None;
          self.window_display = None;
          self.windows(target, ctx);
          ctx.request_repaint();
          return;
        }
      }
      let error = preview.update_texture(ctx);
      let frame_size = if error.is_none() {
        preview.texture.as_ref().map(egui::TextureHandle::size)
      } else {
        None
      };
      let elements = if let Some(semantic) = &mut self.semantic {
        semantic.show(ctx, frame_size);
        semantic.overlay_elements()
      } else {
        &[]
      };
      preview.show(ctx, error.as_deref(), elements);
      return;
    }
    let mut selected = None;
    let mut select_window = None;
    let mut refresh = false;
    egui::CentralPanel::default().show(ctx, |ui| match &self.state {
      State::Discovering(_) => {
        ui.spinner();
        ui.label("Discovering remote displays…");
      }
      State::FindingWindows(_, _) => {
        ui.spinner();
        ui.label("Finding windows…");
      }
      State::ChoosingWindow(target, windows) => {
        if windows.is_empty() {
          ui.label("No capturable windows found.");
        }
        for window in windows {
          let label = if window.title.is_empty() {
            &window.app_id
          } else {
            &window.title
          };
          if ui.button(label).on_hover_text(&window.app_id).clicked() {
            selected = Some((*target, Some(window.clone())));
          }
        }
        select_window = ui.button("Refresh windows").clicked().then_some(*target);
        refresh = ui.button("Back to displays").clicked();
      }
      State::Connecting(_) => {
        ui.spinner();
        ui.label("Connecting to display…");
      }
      State::Selecting(sessions) => {
        if sessions.is_empty() {
          ui.label("No X11 or Wayland displays found.");
        }
        for session in sessions {
          let (label, target) = match &session.display {
            Display::X11(number) => (format!("X11 :{number}"), Some(Target::X11(*number))),
            Display::Wayland { uid, socket } => {
              let class = session.class.as_deref().unwrap_or("unknown session class");
              (
                format!("Wayland {socket} · UID {uid} · {class}"),
                Target::parse(socket).ok(),
              )
            }
          };
          ui.horizontal(|ui| {
            if ui
              .add_enabled(
                session.selectable && target.is_some(),
                egui::Button::new(label),
              )
              .clicked()
              && let Some(target) = target
            {
              selected = Some((target, None));
            }
            if let Some(target) = target
              && ui
                .add_enabled(session.selectable, egui::Button::new("Windows…"))
                .clicked()
            {
              select_window = Some(target);
            }
          });
        }
        refresh = ui.button("Refresh").clicked();
      }
      State::Failed(error) => {
        ui.label(error);
        refresh = ui.button("Discover displays").clicked();
      }
      State::Viewing(_) => unreachable!(),
    });
    if let Some((target, window)) = selected {
      self.connect(target, window, ctx);
    } else if let Some(target) = select_window {
      self.windows(target, ctx);
    } else if refresh {
      self.discover(ctx);
    }
  }
}

fn run<T: Transport + Send + Sync + 'static>(
  title: &str,
  transport: T,
  target: Option<Target>,
  windows: bool,
) -> Result<(), Box<dyn std::error::Error>> {
  let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default()
      .with_inner_size(X11_FRAME_SIZE.map(|size| size as f32)),
    ..Default::default()
  };
  eframe::run_native(
    title,
    options,
    Box::new(move |cc| {
      let mut viewer = Viewer {
        client: Client::new(transport),
        state: State::Selecting(Vec::new()),
        semantic: None,
        window_display: None,
      };
      if let Some(target) = target {
        if windows {
          viewer.windows(target, &cc.egui_ctx);
        } else {
          viewer.connect(target, None, &cc.egui_ctx);
        }
      } else {
        viewer.discover(&cc.egui_ctx);
      }
      Ok(Box::new(viewer))
    }),
  )?;
  Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = env::args().skip(1).collect();
  let (endpoint, target, windows) = match args.as_slice() {
    [endpoint] => (endpoint, None, false),
    [endpoint, display] => (endpoint, Some(Target::parse(display)?), false),
    [endpoint, display, flag] if flag == "--windows" => {
      (endpoint, Some(Target::parse(display)?), true)
    }
    _ => return Err("usage: ctream-preview SSH_HOST|--local [DISPLAY [--windows]]".into()),
  };
  if endpoint == "--local" {
    run("ctream · local", Local::default(), target, windows)
  } else {
    run(
      &format!("ctream · {endpoint}"),
      Ssh(endpoint.clone()),
      target,
      windows,
    )
  }
}
