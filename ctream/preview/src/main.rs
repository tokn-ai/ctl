use ctream_client::{
  Session,
  media::{CaptureLifetime, Frame, VideoSource},
  options::ConnectionOptions,
  transport::Transport,
};
use ctream_proto::{Element, Target, valid_capture_driver, valid_window_id};
mod input;
mod overlay;
mod requests;
mod semantic;
mod view;
mod welcome;

use eframe::egui;
use std::{
  env,
  ffi::OsString,
  io,
  sync::{
    Arc, Mutex,
    mpsc::{self, Receiver},
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
  driver: Option<String>,
}

impl<T: Transport> Preview<T> {
  fn connect(session: Session<T>, driver: Option<&str>, ctx: egui::Context) -> io::Result<Self> {
    let capture = if let Some(driver) = driver {
      session.capture_with_driver(driver)?
    } else {
      session.capture()?
    };
    let driver = capture.driver().map(str::to_owned);
    let (mut source, lifetime) = capture.split();
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
      input: None,
      forwarding: false,
      input_error: None,
      capture_lifetime: Some(lifetime),
      stream,
      texture: None,
      frames: 0,
      driver,
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

  fn stop_control(&mut self) {
    self.input.take();
    self.forwarding = false;
    self.input_error = None;
  }

  fn ready(&self) -> bool {
    self.texture.is_some()
      && self.capture_lifetime.is_some()
      && self.stream.lock().unwrap().error.is_none()
  }

  fn show(
    &mut self,
    ui: &mut egui::Ui,
    error: Option<&str>,
    elements: &[Element],
    interactive: bool,
  ) {
    let ctx = ui.ctx().clone();
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
      // Painting alone does not occupy layout space. Reserve the entire pane,
      // including letterboxing, so surrounding setup controls stay outside it.
      ui.allocate_rect(ui.available_rect_before_wrap(), egui::Sense::hover());
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
        let active =
          interactive && error.is_none() && ctx.input(|i| i.focused) && !ctx.wants_keyboard_input();
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
  }
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

fn run<T: Transport + Send + Sync + 'static>(
  title: &str,
  transport: T,
  target: Option<Target>,
  windows: bool,
  window: Option<String>,
  driver: String,
) -> Result<(), Box<dyn std::error::Error>> {
  let options = eframe::NativeOptions {
    renderer: eframe::Renderer::Wgpu,
    viewport: egui::ViewportBuilder::default()
      .with_inner_size([860.0, 620.0])
      .with_min_inner_size([600.0, 460.0]),
    ..Default::default()
  };
  eframe::run_native(
    title,
    options,
    Box::new(move |cc| {
      let viewer = welcome::Viewer::new(transport, target, windows, window, driver, &cc.egui_ctx);
      Ok(Box::new(viewer))
    }),
  )?;
  Ok(())
}

const USAGE: &str =
  "Usage: ctream-preview [OPTIONS] [SSH_HOST|--local] [DISPLAY [--windows | --window ID]]

  --host SSH_HOST        Alternative to the positional SSH endpoint
  --agent EXECUTABLE     Agent executable; use a mounted Windows path through WSL
  --ssh-program PROGRAM  SSH-compatible launcher (default: ssh)
  --ssh-arg ARG          Launcher prefix argument; repeat as needed
  --window ID           Open a window ID returned by ctream windows
  --capture-driver ID   Capture backend, or auto (default)

DISPLAY is :DISPLAY, wayland-N, or windows-N. Without an endpoint, use the local agent.
Selections open a view-only setup preview. Start opens the viewer and enables control.";

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
  driver: String,
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
  let mut driver = None;
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
      "--capture-driver" if driver.is_none() => {
        let value = arguments
          .next()
          .ok_or_else(|| invalid("--capture-driver needs an ID"))?;
        let value = value
          .to_str()
          .filter(|id| valid_capture_driver(id))
          .ok_or_else(|| invalid("invalid capture driver ID"))?;
        driver = Some(value.to_owned());
      }
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
  let endpoint = endpoint.unwrap_or(Endpoint::Local);
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
    driver: driver.unwrap_or_else(|| "auto".into()),
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
      options.driver,
    ),
    Endpoint::Ssh(host) => run(
      &format!("ctream · {host}"),
      options.connection.ssh(&host)?,
      options.target,
      options.windows,
      options.window,
      options.driver,
    ),
  }
}

#[cfg(test)]
mod test_support {
  use super::*;
  use ctream_client::Client;
  use ctream_client::transport::{Cancellation, Channel};
  use ctream_proto::Window;
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
      let control = session.input().unwrap();
      let mut preview = Preview::connect(session, None, context.clone()).unwrap();
      preview.input = Some(input::Input::new(control));
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
    let options = parse(args(&[])).unwrap().unwrap();
    assert!(matches!(options.endpoint, Endpoint::Local));
    assert_eq!(options.driver, "auto");
    let options = parse(args(&[
      "--local",
      "--capture-driver",
      "windows.wgc",
      "windows-2",
      "--windows",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(options.driver, "windows.wgc");
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
      vec!["--local", "--capture-driver"],
      vec!["--local", "--capture-driver", "bad;driver"],
      vec![
        "--local",
        "--capture-driver",
        "auto",
        "--capture-driver",
        "windows.wgc",
      ],
    ] {
      assert!(parse(args(&arguments)).is_err(), "{arguments:?}");
    }
  }
}
