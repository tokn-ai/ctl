use ctream_client::{
  Session,
  media::{CaptureLifetime, Frame},
  options::ConnectionOptions,
  transport::Transport,
};
use ctream_proto::{Element, Target, valid_capture_driver, valid_window_id};
mod agent_sync;
mod chrome;
mod drivers;
mod fonts;
mod input;
mod latency;
#[cfg(target_os = "macos")]
mod macos_chrome;
#[cfg(target_os = "macos")]
mod macos_local;
mod overlay;
mod requests;
mod semantic;
mod setup;
mod stats;
mod view;
mod welcome;
mod workspace;
mod workspace_hosts;

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
  frame: Option<Frame>,
  spare: Frame,
  end: Option<CaptureEnd>,
}

#[derive(Clone)]
enum CaptureEnd {
  Finished,
  Failed(String),
}

struct Preview<T: Transport> {
  input: Option<input::Input<ctream_client::input::Input<T::Reader, T::Writer>>>,
  input_error: Option<String>,
  capture_lifetime: Option<CaptureLifetime<T::Writer>>,
  stream: Arc<Mutex<Stream>>,
  texture: Option<egui::TextureHandle>,
  frames: u64,
  driver: Option<String>,
}

enum PreviewMode<'a> {
  Setup,
  Viewer {
    interactive: bool,
    pointer_regions: &'a [egui::Rect],
  },
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
    let repaint = view::capture_repaint(&ctx);
    thread::spawn(move || {
      let mut frame = Frame::default();
      let end = loop {
        match source.try_read_frame(&mut frame) {
          Ok(true) => {
            let mut stream = reader.lock().unwrap();
            let previous = stream.frame.replace(frame);
            let wake = previous.is_none();
            frame = previous.unwrap_or_else(|| std::mem::take(&mut stream.spare));
            drop(stream);
            if wake {
              let _ = repaint.try_send(());
            }
          }
          Ok(false) => break CaptureEnd::Finished,
          Err(error) => {
            break CaptureEnd::Failed(format!(
              "Capture ended ({error}). See terminal for remote diagnostics."
            ));
          }
        }
      };
      reader.lock().unwrap().end = Some(end);
      let _ = repaint.try_send(());
    });
    Ok(Self {
      input: None,
      input_error: None,
      capture_lifetime: Some(lifetime),
      stream,
      texture: None,
      frames: 0,
      driver,
    })
  }

  fn update_texture(&mut self, ctx: &egui::Context) -> Option<CaptureEnd> {
    let (frame, end) = {
      let mut stream = self.stream.lock().unwrap();
      (stream.frame.take(), stream.end.clone())
    };
    if let Some(frame) = frame {
      let image = egui::ColorImage::from_rgb(frame.size, &frame.pixels);
      if end.is_none() {
        self.stream.lock().unwrap().spare = frame;
      }
      view::upload(ctx, &mut self.texture, image);
      self.frames += 1;
      if self.frames == 2 {
        eprintln!("Live preview received two frames on the wgpu renderer.");
      }
    }
    if end.is_some() {
      // Capture and input use independent channels. End control as soon as
      // capture stops, even if the viewer still has a texture to show.
      self.input.take();
      self.capture_lifetime.take();
    }
    end
  }

  fn stop_control(&mut self) {
    self.input.take();
    self.input_error = None;
  }

  fn ready(&self) -> bool {
    self.texture.is_some()
      && self.capture_lifetime.is_some()
      && self.stream.lock().unwrap().end.is_none()
  }

  fn image_size(&self) -> Option<[u32; 2]> {
    self
      .input
      .as_ref()
      .and_then(input::Input::desktop_size)
      .or_else(|| {
        self
          .texture
          .as_ref()
          .map(|texture| texture.size().map(|v| v as u32))
      })
  }

  fn show(
    &mut self,
    ui: &mut egui::Ui,
    error: Option<&str>,
    elements: &[Element],
    mode: PreviewMode<'_>,
  ) -> Option<egui::Rect> {
    let ctx = ui.ctx().clone();
    let (interactive, pointer_regions) = match mode {
      PreviewMode::Setup => {
        if let Some(error) = error {
          ui.label(error);
        } else if self.texture.is_none() {
          ui.spinner();
          ui.label("Waiting for the first frame…");
        }
        (false, &[][..])
      }
      PreviewMode::Viewer {
        interactive,
        pointer_regions,
      } => (interactive, pointer_regions),
    };
    if let (Some(texture), Some(size)) = (&self.texture, self.image_size()) {
      let area = ui.available_rect_before_wrap();
      let image = match mode {
        PreviewMode::Setup => view::fit(area, size),
        // Fill the video canvas while the OS applies the aspect correction or
        // gives the window a fixed size. Keep the full frame and input mapping.
        PreviewMode::Viewer { .. } => view::fill(area, size),
      };
      if interactive
        && let Some(input) = &mut self.input
        && let Err(err) = input.update(
          &ctx,
          image.as_ref().filter(|_| error.is_none()),
          pointer_regions,
        )
      {
        self.input = None;
        self.input_error = Some(format!("Input disconnected ({err})"));
      }
      return image.map(|image| {
        let rect = view::paint(ui, texture, area, &image);
        if error.is_none() {
          overlay::paint(ui.painter(), &image, elements);
        }
        rect
      });
    }
    None
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
    ctx.request_repaint_of(egui::ViewportId::ROOT);
  });
  receiver
}

fn run(title: &str, create: eframe::AppCreator<'static>) -> Result<(), Box<dyn std::error::Error>> {
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
      fonts::install(&cc.egui_ctx);
      create(cc)
    }),
  )?;
  Ok(())
}

const USAGE: &str =
  "Usage: ctream-preview [OPTIONS] [SSH_HOST|--local] [TARGET] [--windows | --window ID]

  --host SSH_HOST        Alternative to the positional SSH endpoint
  --agent EXECUTABLE     Agent executable; use a mounted Windows path through WSL
  --ssh-program PROGRAM  SSH-compatible launcher (default: ssh)
  --ssh-arg ARG          Launcher prefix argument; repeat as needed
  --windows             Start with the window picker across available sessions
  --window ID           Open a window ID returned by ctream windows
  --capture-driver ID   Capture backend, or auto (default)

TARGET is :DISPLAY, wayland-N, windows-N, or the desktop window lists windows and macos.
Without an endpoint, choose a workspace host.
Use --local for local capture.
Selections open a view-only setup preview. Start hides setup and opens a separate viewer.
Hover over the viewer's top edge for window controls. Closing it releases control and restores setup.";

enum Endpoint {
  Workspace,
  Local,
  Ssh(String),
}

#[derive(Clone)]
struct SourceOptions {
  target: Option<Target>,
  windows: bool,
  window: Option<String>,
  driver: String,
}

impl SourceOptions {
  fn viewer<T: Transport + Send + Sync + 'static>(
    self,
    transport: T,
    ctx: &egui::Context,
  ) -> welcome::Viewer<T> {
    welcome::Viewer::new(
      transport,
      self.target,
      self.windows,
      self.window,
      self.driver,
      ctx,
    )
  }
}

struct Options {
  endpoint: Endpoint,
  connection: ConnectionOptions,
  source: SourceOptions,
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
  let endpoint = endpoint.unwrap_or_else(|| {
    if connection.agent_is_explicit() {
      Endpoint::Local
    } else {
      Endpoint::Workspace
    }
  });
  if window.is_some() && target.is_none() {
    return Err(invalid("--window requires a target"));
  }
  if windows && window.is_some() {
    return Err(invalid("choose either --windows or --window ID"));
  }
  match &endpoint {
    Endpoint::Workspace | Endpoint::Local => {
      connection.local()?;
    }
    Endpoint::Ssh(host) => {
      connection.ssh(host)?;
    }
  }
  Ok(Some(Options {
    endpoint,
    connection,
    source: SourceOptions {
      target,
      windows,
      window,
      driver: driver.unwrap_or_else(|| "auto".into()),
    },
  }))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
  let Some(options) = parse(env::args_os().skip(1)).map_err(|error| format!("{error}\n{USAGE}"))?
  else {
    println!("{USAGE}");
    return Ok(());
  };
  let Options {
    endpoint,
    connection,
    source,
  } = options;
  match endpoint {
    Endpoint::Workspace => run(
      "ctream",
      Box::new(move |cc| Ok(Box::new(workspace::Preview::new(source, &cc.egui_ctx)))),
    ),
    Endpoint::Local => {
      #[cfg(target_os = "macos")]
      if !connection.agent_is_explicit() {
        if source.target.is_some()
          || !matches!(source.driver.as_str(), "auto" | macos_local::DRIVER)
        {
          return Err(invalid("Use the system picker to select a local macOS window, or specify --agent for a custom local agent.").into());
        }
        return run(
          "ctream · local",
          Box::new(|cc| Ok(Box::new(macos_local::Viewer::new(&cc.egui_ctx)))),
        );
      }
      let transport = connection.local()?;
      run(
        "ctream · local",
        Box::new(move |cc| Ok(Box::new(source.viewer(transport, &cc.egui_ctx)))),
      )
    }
    Endpoint::Ssh(host) => {
      let transport = connection.ssh(&host)?;
      run(
        &format!("ctream · {host}"),
        Box::new(move |cc| Ok(Box::new(source.viewer(transport, &cc.egui_ctx)))),
      )
    }
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
  fn capture_drains_to_the_latest_frame_while_repaint_is_blocked() {
    let (session, _) = test_support::session(b"P6\n1 1\n255\nabcP6\n2 1\n255\ndefghi");
    let ctx = egui::Context::default();
    let mut preview = ctx.input(|_| {
      // Hold egui's context lock while the capture reader reaches EOF.
      // Waiting for a repaint in the reader would stop it at the first frame.
      let preview = Preview::connect(session, None, ctx.clone()).unwrap();
      let deadline = Instant::now() + Duration::from_secs(2);
      while preview.stream.lock().unwrap().end.is_none() {
        assert!(Instant::now() < deadline, "capture waited for the UI lock");
        thread::sleep(Duration::from_millis(1));
      }
      preview
    });
    assert!(matches!(
      preview.update_texture(&ctx),
      Some(CaptureEnd::Finished)
    ));
    assert_eq!(preview.texture.as_ref().unwrap().size(), [2, 1]);
    assert_eq!(preview.frames, 1);
  }

  #[test]
  fn capture_end_closes_both_channels_before_preview_is_dropped() {
    for bytes in [&b"P6\n1 1\n255\nabc"[..], &b""[..]] {
      let (session, observed) = test_support::session(bytes);
      let context = egui::Context::default();
      let control = session.input().unwrap();
      let mut preview = Preview::connect(session, None, context.clone()).unwrap();
      preview.input = Some(input::Input::new(control));
      let deadline = Instant::now() + Duration::from_secs(2);
      while preview.stream.lock().unwrap().end.is_none() {
        assert!(Instant::now() < deadline, "capture worker did not finish");
        thread::sleep(Duration::from_millis(1));
      }
      assert!(observed.lifecycle.lock().unwrap().is_empty());
      assert!(matches!(
        preview.update_texture(&context),
        Some(CaptureEnd::Finished)
      ));
      assert!(preview.input.is_none());
      assert!(preview.capture_lifetime.is_none());
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
      assert!(matches!(options.source.target, Some(Target::Windows(2))));
      assert!(!options.source.windows);
    }
    let options = parse(args(&["--local", "--agent", "agent", ":1"]))
      .unwrap()
      .unwrap();
    assert!(matches!(options.endpoint, Endpoint::Local));
    let options = parse(args(&["test-host", "windows-2", "--windows"]))
      .unwrap()
      .unwrap();
    assert!(options.source.windows);
    let options = parse(args(&["test-host", "--windows"])).unwrap().unwrap();
    assert!(options.source.windows && options.source.target.is_none());
    let options = parse(args(&["test-host", "windows-2", "--window", "win-123"]))
      .unwrap()
      .unwrap();
    assert_eq!(options.source.window.as_deref(), Some("win-123"));
    let options = parse(args(&[])).unwrap().unwrap();
    assert!(matches!(options.endpoint, Endpoint::Workspace));
    assert_eq!(options.source.driver, "auto");
    let options = parse(args(&[
      "--local",
      "--capture-driver",
      "windows.wgc",
      "windows-2",
      "--windows",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(options.source.driver, "windows.wgc");
  }

  #[test]
  fn unsupported_and_conflicting_preview_modes_are_rejected() {
    for arguments in [
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
