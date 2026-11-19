mod capture;
use ctream::{
  Display,
  protocol::{Session, Target, X11_FRAME_SIZE},
  transport::{self, Channel, Request, Ssh, Transport},
};
mod input;

use eframe::egui;
use std::{
  env,
  io::{self, BufReader, Write},
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

struct Preview {
  input: Option<input::Input>,
  forwarding: bool,
  input_error: Option<String>,
  _capture_lifetime: Box<dyn Write + Send>,
  stream: Arc<Mutex<Stream>>,
  texture: Option<egui::TextureHandle>,
  frames: u64,
}

impl Preview {
  fn connect(transport: &dyn Transport, target: Target, ctx: egui::Context) -> io::Result<Self> {
    let (input, input_error) = match input::Input::connect(transport, target) {
      Ok(input) => (Some(input), None),
      Err(error) => (None, Some(format!("View only ({error})"))),
    };
    let Channel { reader, writer } = transport.open(Request::Capture(target))?;
    let mut stdout = BufReader::new(reader);
    let stream = Arc::new(Mutex::new(Stream::default()));
    let reader = Arc::clone(&stream);
    thread::spawn(move || {
      let mut buffer = Vec::new();
      loop {
        let image = match capture::read_frame(&mut stdout, &target, &mut buffer) {
          Ok(image) => image,
          Err(error) => {
            reader.lock().unwrap().error = Some(format!(
              "Capture ended ({error}). See terminal for remote diagnostics."
            ));
            ctx.request_repaint();
            break;
          }
        };
        reader.lock().unwrap().frame = Some(image);
        ctx.request_repaint();
      }
    });
    Ok(Self {
      input,
      forwarding: false,
      input_error,
      _capture_lifetime: writer,
      stream,
      texture: None,
      frames: 0,
    })
  }
}

impl eframe::App for Preview {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
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
    egui::CentralPanel::default().show(ctx, |ui| {
      if let Some(error) = error.as_ref().or(self.input_error.as_ref()) {
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
        let rect = input::image_rect(ui.available_rect_before_wrap(), size);
        ui.painter().image(
          texture.id(),
          rect,
          egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
          egui::Color32::WHITE,
        );
        if let Some(input) = &mut self.input
          && ctx.current_pass_index() == 0
          && self.input_error.is_none()
        {
          let active = error.is_none() && ctx.input(|i| i.focused);
          let result = if active {
            ctx.input(|i| input.forward(rect, size, &i.raw.events, i.modifiers))
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

enum State {
  Discovering(Receiver<io::Result<Vec<Session>>>),
  Selecting(Vec<Session>),
  Connecting(Receiver<io::Result<Preview>>),
  Viewing(Preview),
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

struct Viewer {
  transport: Arc<dyn Transport + Send + Sync>,
  state: State,
}

impl Viewer {
  fn discover(&mut self, ctx: &egui::Context) {
    let transport = Arc::clone(&self.transport);
    self.state = State::Discovering(background(ctx, move || {
      transport::discover(transport.as_ref())
    }));
  }

  fn connect(&mut self, target: Target, ctx: &egui::Context) {
    let transport = Arc::clone(&self.transport);
    let render = ctx.clone();
    self.state = State::Connecting(background(ctx, move || {
      Preview::connect(transport.as_ref(), target, render)
    }));
  }
}

fn receive<T>(
  result: Result<io::Result<T>, TryRecvError>,
  success: impl FnOnce(T) -> State,
) -> Option<State> {
  match result {
    Ok(Ok(value)) => Some(success(value)),
    Ok(Err(error)) => Some(State::Failed(error.to_string())),
    Err(TryRecvError::Empty) => None,
    Err(TryRecvError::Disconnected) => Some(State::Failed("Agent connection task ended".into())),
  }
}

impl eframe::App for Viewer {
  fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
    let next = match &self.state {
      State::Discovering(result) => receive(result.try_recv(), State::Selecting),
      State::Connecting(result) => receive(result.try_recv(), State::Viewing),
      _ => None,
    };
    if let Some(next) = next {
      self.state = next;
    }
    if let State::Viewing(preview) = &mut self.state {
      preview.update(ctx, frame);
      return;
    }
    let mut selected = None;
    let mut refresh = false;
    egui::CentralPanel::default().show(ctx, |ui| match &self.state {
      State::Discovering(_) => {
        ui.spinner();
        ui.label("Discovering remote displays…");
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
          if ui
            .add_enabled(
              session.selectable && target.is_some(),
              egui::Button::new(label),
            )
            .clicked()
          {
            selected = target;
          }
        }
        refresh = ui.button("Refresh").clicked();
      }
      State::Failed(error) => {
        ui.label(error);
        refresh = ui.button("Discover displays").clicked();
      }
      State::Viewing(_) => unreachable!(),
    });
    if let Some(target) = selected {
      self.connect(target, ctx);
    } else if refresh {
      self.discover(ctx);
    }
  }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args: Vec<_> = env::args().skip(1).collect();
  let (host, target) = match args.as_slice() {
    [host] => (host, None),
    [host, display] => (host, Some(Target::parse(display)?)),
    _ => return Err("usage: ctream-preview SSH_HOST [DISPLAY]".into()),
  };
  let title = format!("ctream · {host}");
  let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default()
      .with_inner_size(X11_FRAME_SIZE.map(|size| size as f32)),
    ..Default::default()
  };
  eframe::run_native(
    &title,
    options,
    Box::new(|cc| {
      let transport: Arc<dyn Transport + Send + Sync> = Arc::new(Ssh(host.clone()));
      let mut viewer = Viewer {
        transport,
        state: State::Selecting(Vec::new()),
      };
      if let Some(target) = target {
        viewer.connect(target, &cc.egui_ctx);
      } else {
        viewer.discover(&cc.egui_ctx);
      }
      Ok(Box::new(viewer))
    }),
  )?;
  Ok(())
}
