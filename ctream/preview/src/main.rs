use ctream_client::{
  Client, Session,
  media::{CaptureLifetime, Frame, VideoSource},
  options::ConnectionOptions,
  transport::Transport,
};
use ctream_proto::{
  Display, Element, Session as DisplaySession, Target, Window, X11_FRAME_SIZE, valid_window_id,
};
mod input;
mod overlay;
mod semantic;
mod view;

use eframe::egui;
use std::{
  env,
  ffi::OsString,
  io,
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
  capture_lifetime: Option<CaptureLifetime<T::Writer>>,
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
      capture_lifetime: Some(lifetime),
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
    if error.is_some() {
      // Capture and input use independent channels. A failed capture must not
      // leave an input agent alive behind a frozen preview.
      self.input.take();
      self.capture_lifetime.take();
      self.forwarding = false;
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
          ui.label("No displays found.");
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
            Display::Windows {
              number,
              name,
              bounds,
            } => (
              format!(
                "Windows windows-{number} · {name} · {}×{}",
                i64::from(bounds[2]) - i64::from(bounds[0]),
                i64::from(bounds[3]) - i64::from(bounds[1]),
              ),
              Some(Target::Windows(*number)),
            ),
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
  window: Option<String>,
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
          viewer.connect(
            target,
            window.map(|id| Window {
              id,
              ..Default::default()
            }),
            &cc.egui_ctx,
          );
        }
      } else {
        viewer.discover(&cc.egui_ctx);
      }
      Ok(Box::new(viewer))
    }),
  )?;
  Ok(())
}

const USAGE: &str =
  "Usage: ctream-preview [OPTIONS] SSH_HOST|--local [DISPLAY [--windows | --window ID]]

  --host SSH_HOST        Alternative to the positional SSH endpoint
  --agent EXECUTABLE     Agent executable; use a mounted Windows path through WSL
  --ssh-program PROGRAM  SSH-compatible launcher (default: ssh)
  --ssh-arg ARG          Launcher prefix argument; repeat as needed
  --window ID           Open a window ID returned by ctream windows

DISPLAY is :DISPLAY, wayland-N, or windows-N. Omit it to discover displays.
Use --windows to choose an individual window on the display.";

enum Endpoint {
  Local,
  Ssh(String),
}

struct Options {
  endpoint: Endpoint,
  connection: ConnectionOptions,
  target: Option<Target>,
  windows: bool,
  window: Option<String>,
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Option<Options>> {
  let mut arguments = arguments.into_iter();
  let mut endpoint = None;
  let mut target = None;
  let mut windows = false;
  let mut window = None;
  let mut connection = ConnectionOptions::default();
  while let Some(argument) = arguments.next() {
    let argument = argument
      .to_str()
      .ok_or_else(|| invalid("invalid preview argument"))?;
    match argument {
      "--help" | "-h" => return Ok(None),
      "--local" if endpoint.is_none() => endpoint = Some(Endpoint::Local),
      "--host" if endpoint.is_none() => {
        let host = arguments
          .next()
          .ok_or_else(|| invalid("--host needs an SSH destination"))?;
        let host = host
          .to_str()
          .filter(|host| !host.is_empty() && !host.starts_with('-'))
          .ok_or_else(|| invalid("--host needs an SSH destination"))?;
        endpoint = Some(Endpoint::Ssh(host.to_owned()));
      }
      "--windows" if !windows => windows = true,
      "--window" if window.is_none() => {
        let id = arguments
          .next()
          .ok_or_else(|| invalid("--window needs a window ID"))?;
        let id = id
          .to_str()
          .filter(|id| valid_window_id(id))
          .ok_or_else(|| invalid("invalid window ID"))?;
        window = Some(id.to_owned());
      }
      option if connection.parse_option(option, &mut arguments)? => {}
      value if !value.starts_with('-') && endpoint.is_none() => {
        endpoint = Some(Endpoint::Ssh(value.to_owned()))
      }
      value if !value.starts_with('-') && target.is_none() => {
        target = Some(Target::parse(value).map_err(invalid)?)
      }
      _ => return Err(invalid("unexpected preview argument")),
    }
  }
  let endpoint = endpoint.ok_or_else(|| invalid("expected an SSH endpoint or --local"))?;
  if (windows || window.is_some()) && target.is_none() {
    return Err(invalid("window selection requires a display"));
  }
  if windows && window.is_some() {
    return Err(invalid("choose either --windows or --window ID"));
  }
  match &endpoint {
    Endpoint::Local => {
      connection.local()?;
    }
    Endpoint::Ssh(host) => {
      connection.ssh(host)?;
    }
  }
  Ok(Some(Options {
    endpoint,
    connection,
    target,
    windows,
    window,
  }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let Some(options) = parse(env::args_os().skip(1)).map_err(|error| format!("{error}\n{USAGE}"))?
  else {
    println!("{USAGE}");
    return Ok(());
  };
  match options.endpoint {
    Endpoint::Local => run(
      "ctream · local",
      options.connection.local()?,
      options.target,
      options.windows,
      options.window,
    ),
    Endpoint::Ssh(host) => run(
      &format!("ctream · {host}"),
      options.connection.ssh(&host)?,
      options.target,
      options.windows,
      options.window,
    ),
  }
}

#[cfg(test)]
mod test_support {
  use super::*;
  use ctream_client::transport::{Cancellation, Channel};
  use std::io::{Cursor, Write};

  #[derive(Default)]
  pub struct Observed {
    pub bytes: Mutex<Vec<u8>>,
    pub lifecycle: Mutex<Vec<String>>,
  }

  pub struct Writer {
    operation: String,
    observed: Arc<Observed>,
  }

  impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self.observed.bytes.lock().unwrap().extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl Drop for Writer {
    fn drop(&mut self) {
      self
        .observed
        .lifecycle
        .lock()
        .unwrap()
        .push(format!("close {}", self.operation));
    }
  }

  pub struct Mock {
    observed: Arc<Observed>,
    capture: &'static [u8],
  }

  impl Transport for Mock {
    type Reader = Cursor<&'static [u8]>;
    type Writer = Writer;

    fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
      let operation = arguments[1].clone();
      let response = match operation.as_str() {
        "input" | "input-window" => &b"ready\n"[..],
        "capture" | "capture-window" => self.capture,
        _ => return Err(io::Error::other("unexpected operation")),
      };
      let observed = Arc::clone(&self.observed);
      let cancelled = operation.clone();
      Ok(Channel {
        reader: Cursor::new(response),
        writer: Writer {
          operation,
          observed: Arc::clone(&observed),
        },
        cancellation: Cancellation::new(move || {
          observed
            .lifecycle
            .lock()
            .unwrap()
            .push(format!("cancel {cancelled}"));
        }),
      })
    }
  }

  pub fn session(capture: &'static [u8]) -> (Session<Mock>, Arc<Observed>) {
    let observed = Arc::new(Observed::default());
    let client = Client::new(Mock {
      observed: Arc::clone(&observed),
      capture,
    });
    (
      client.session(
        Target::Windows(2),
        Some(Window {
          id: "window_1".into(),
          ..Default::default()
        }),
      ),
      observed,
    )
  }
}

#[cfg(test)]
mod lifecycle_tests {
  use super::*;
  use std::time::{Duration, Instant};

  #[test]
  fn terminal_capture_error_closes_both_channels_before_preview_is_dropped() {
    for bytes in [&b"P6\n1 1\n255\nabc"[..], &b""[..]] {
      let (session, observed) = test_support::session(bytes);
      let context = egui::Context::default();
      let mut preview = Preview::connect(session, context.clone()).unwrap();
      preview.forwarding = true;
      let deadline = Instant::now() + Duration::from_secs(2);
      while preview.stream.lock().unwrap().error.is_none() {
        assert!(Instant::now() < deadline, "capture worker did not finish");
        thread::sleep(Duration::from_millis(1));
      }
      assert!(observed.lifecycle.lock().unwrap().is_empty());
      assert!(
        preview
          .update_texture(&context)
          .unwrap()
          .contains("Capture ended")
      );
      assert!(preview.input.is_none());
      assert!(preview.capture_lifetime.is_none());
      assert!(!preview.forwarding);
      assert_eq!(preview.texture.is_some(), !bytes.is_empty());
      let expected = [
        "close input-window",
        "cancel input-window",
        "close capture-window",
        "cancel capture-window",
      ];
      assert_eq!(*observed.lifecycle.lock().unwrap(), expected);
      assert!(preview.update_texture(&context).is_some());
      drop(preview);
      assert_eq!(*observed.lifecycle.lock().unwrap(), expected);
    }
  }
}

#[cfg(test)]
mod argument_tests {
  use super::*;

  fn args(arguments: &[&str]) -> Vec<OsString> {
    arguments.iter().map(OsString::from).collect()
  }

  #[test]
  fn positional_and_explicit_hosts_support_windows_agent_launching() {
    for arguments in [
      vec![
        "test-host",
        "windows-2",
        "--agent",
        "/mounted drive/agent.exe",
      ],
      vec![
        "--host",
        "test-host",
        "--ssh-program",
        "ctl",
        "--ssh-arg",
        "ssh",
        "windows-2",
      ],
    ] {
      let options = parse(args(&arguments)).unwrap().unwrap();
      assert!(matches!(options.endpoint, Endpoint::Ssh(_)));
      assert!(matches!(options.target, Some(Target::Windows(2))));
      assert!(!options.windows);
    }
    let options = parse(args(&["--local", "--agent", "agent", ":1"]))
      .unwrap()
      .unwrap();
    assert!(matches!(options.endpoint, Endpoint::Local));
    let options = parse(args(&["test-host", "windows-2", "--windows"]))
      .unwrap()
      .unwrap();
    assert!(options.windows);
    let options = parse(args(&["test-host", "windows-2", "--window", "win-123"]))
      .unwrap()
      .unwrap();
    assert_eq!(options.window.as_deref(), Some("win-123"));
  }

  #[test]
  fn unsupported_and_conflicting_preview_modes_are_rejected() {
    for arguments in [
      vec!["test-host", "--windows"],
      vec!["test-host", "--window", "win-1"],
      vec!["test-host", "windows-2", "--windows", "--window", "win-1"],
      vec!["test-host", "windows-2", "--window", "bad/id"],
      vec!["--local", "--ssh-program", "ctl"],
      vec!["--host", "test-host", "--local"],
      vec!["--host"],
    ] {
      assert!(parse(args(&arguments)).is_err(), "{arguments:?}");
    }
  }
}
