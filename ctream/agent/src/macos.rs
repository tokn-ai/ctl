mod leases;
mod socket;

use self::{
  leases::{CaptureRegistry, CaptureSession},
  socket::{Listener, Response},
};
use core_foundation::{
  base::{TCFType, kCFAllocatorDefault},
  runloop::{
    CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, CFRunLoopSourceCreate,
    kCFRunLoopDefaultMode,
  },
};
use ctream_agent::{
  capture,
  protocol::{Request, TargetOperation, parse},
  runtime::{capture_started, capture_video_started, input_capabilities, video_capabilities},
};
use ctream_driver::{
  CaptureCancellation, CaptureLifetime, CaptureOptions, CaptureSource, Frame, Result,
};
use ctream_driver_capture::macos::screen_capture_kit::{self, Capture, Event};
use ctream_driver_input::macos::{WindowIdentity, WindowInput, has_access};
use ctream_proto::{
  CaptureFormat, Display, PING_HELLO, PING_TOKEN, Session, Target, VideoRequest, Window, WindowId,
};
use dispatch2::{DispatchQueue, MainThreadBound, run_on_main};
use std::{
  ffi::c_void,
  io::{self, BufRead, BufReader, Read, Write},
  net::Shutdown,
  os::unix::net::UnixStream,
  ptr,
  sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
  },
  thread,
  time::Duration,
};

pub fn run(arguments: &[String]) -> Result<()> {
  if arguments == ["serve"] {
    return serve();
  }
  // The SSH-side process owns the action journal. Native window access
  // continues to run in the logged-in user's GUI broker.
  match parse(arguments)? {
    Request::AttributedSession {
      target,
      agent_id,
      window,
    } => crate::attributed::run(target, agent_id, window),
    _ => Ok(socket::relay(arguments)?),
  }
}

fn serve() -> Result<()> {
  if !Capture::is_supported() {
    return Err("ctream-agent serve requires macOS 14 or later".into());
  }
  let listener = Listener::bind()?;
  if !Capture::has_screen_recording_access() {
    eprintln!("ctream-agent needs Screen Recording permission to list or capture macOS windows");
  }
  if !has_access() {
    eprintln!("ctream-agent needs Accessibility permission to control macOS windows");
  }

  // Keep the real main thread in a CF run loop. dispatch_main can leave its
  // original thread, while ScreenCaptureKit checks that thread explicitly.
  extern "C" fn keep_alive(_: *const c_void) {}
  let mut context = CFRunLoopSourceContext {
    version: 0,
    info: ptr::null_mut(),
    retain: None,
    release: None,
    copyDescription: None,
    equal: None,
    hash: None,
    schedule: None,
    cancel: None,
    perform: keep_alive,
  };
  // SAFETY: CF copies the context, and its sole callback carries no data.
  let source = unsafe { CFRunLoopSourceCreate(kCFAllocatorDefault, 0, &mut context) };
  if source.is_null() {
    return Err("could not initialize the macOS desktop agent run loop".into());
  }
  // SAFETY: the nonnull created source belongs to this caller.
  let source = unsafe { CFRunLoopSource::wrap_under_create_rule(source) };
  let run_loop = CFRunLoop::get_current();
  // SAFETY: kCFRunLoopDefaultMode is a constant exported by CoreFoundation.
  run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });
  let server = thread::spawn(move || {
    let result = listener.run(handle);
    DispatchQueue::main().exec_async(move || run_loop.stop());
    result
  });
  eprintln!("ctream-agent is serving the logged-in macOS desktop");
  CFRunLoop::run_current();
  server
    .join()
    .map_err(|_| io::Error::other("desktop ctream agent listener stopped unexpectedly"))??;
  Ok(())
}

fn handle(
  arguments: &[String],
  input: &mut BufReader<UnixStream>,
  output: &mut Response,
) -> Result<()> {
  match parse(arguments)? {
    Request::Ping => ping(input, output),
    Request::InputCapabilities => input_capabilities(output),
    Request::InputSync(Target::MacosDesktop, Some(id)) => {
      forward_input(id, None, input, output, true)
    }
    Request::SessionCapture { window, lease } => {
      stream_window(window, Some(lease), WindowStream::Raw, input, output)
    }
    Request::SessionInput { window, lease } => {
      forward_input(window, Some(lease), input, output, true)
    }
    Request::VideoCapabilities => video_capabilities(output),
    Request::Discover { macos, .. } => {
      let sessions = if macos {
        vec![Session {
          display: Display::MacosDesktop,
          class: Some("macOS desktop".into()),
          selectable: true,
        }]
      } else {
        Vec::new()
      };
      serde_json::to_writer(output, &sessions)?;
      Ok(())
    }
    Request::CaptureCatalog { macos } => {
      let drivers = if macos {
        capture::catalog()
      } else {
        Vec::new()
      };
      serde_json::to_writer(output, &drivers)?;
      Ok(())
    }
    Request::CaptureDrivers(Target::MacosDesktop) => {
      serde_json::to_writer(output, &capture::list(Target::MacosDesktop))?;
      Ok(())
    }
    Request::Target(Target::MacosDesktop, TargetOperation::Windows) => {
      let windows: Vec<_> = list_windows()?
        .into_iter()
        .filter_map(|window| {
          Some(Window {
            id: WindowId::macos(window.id, window.pid)?.into(),
            title: window.title,
            app_id: window.application,
          })
        })
        .collect();
      serde_json::to_writer(output, &windows)?;
      Ok(())
    }
    Request::CaptureWithDriver {
      target: Target::MacosDesktop,
      driver,
      window: Some(id),
    } => {
      let selection = capture::select(Target::MacosDesktop, driver, true)?;
      stream_window(
        id,
        None,
        WindowStream::WithMetadata(selection.id),
        input,
        output,
      )
    }
    Request::CaptureVideo {
      target: Target::MacosDesktop,
      driver,
      window: Some(id),
      request,
    } => {
      let selection = capture::select(Target::MacosDesktop, driver, true)?;
      stream_window(
        id,
        None,
        WindowStream::Video(selection.id, request),
        input,
        output,
      )
    }
    Request::Target(Target::MacosDesktop, TargetOperation::CaptureWindow(id)) => {
      stream_window(id, None, WindowStream::Raw, input, output)
    }
    Request::Target(Target::MacosDesktop, TargetOperation::InputWindow(id)) => {
      forward_input(id, None, input, output, false)
    }
    _ => Err("the macOS desktop agent supports macOS windows".into()),
  }
}

fn ping(input: &mut impl Read, output: &mut impl Write) -> Result<()> {
  output.write_all(&[PING_HELLO])?;
  output.flush()?;
  loop {
    let mut token = [0];
    match input.read_exact(&mut token) {
      Ok(()) if token == [PING_TOKEN] => {
        output.write_all(&token)?;
        output.flush()?;
      }
      Ok(()) => return Err("invalid ping token".into()),
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
      Err(error) => return Err(error.into()),
    }
  }
}

fn list_windows() -> io::Result<Vec<screen_capture_kit::Window>> {
  let (reply, result) = mpsc::sync_channel(1);
  DispatchQueue::main().exec_async(move || {
    let finished = reply.clone();
    if let Err(error) = screen_capture_kit::list_windows(move |windows| {
      let _ = finished.send(windows);
    }) {
      let _ = reply.send(Err(error));
    }
  });
  result
    .recv_timeout(Duration::from_secs(10))
    .map_err(|_| io::Error::other("ScreenCaptureKit did not return a window list"))?
}

fn window_identity(id: &str) -> io::Result<WindowIdentity> {
  let (id, pid) = WindowId::macos_parts(id).ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      "invalid macOS window identifier",
    )
  })?;
  Ok(WindowIdentity { id, pid })
}

static CAPTURES: CaptureRegistry = CaptureRegistry::new();

#[derive(Default)]
struct CaptureEvents {
  latest: Option<Frame>,
  finished: Option<io::Result<()>>,
}

#[derive(Default)]
struct Frames {
  events: Mutex<CaptureEvents>,
  ready: Condvar,
}

impl Frames {
  fn publish(&self, event: Event) {
    let mut events = self.events.lock().unwrap();
    if events.finished.is_some() {
      return;
    }
    match event {
      Event::Frame { size, pixels } => events.latest = Some(Frame { size, pixels }),
      Event::Cancelled | Event::Finished => events.finished = Some(Ok(())),
      Event::Failed(error) => events.finished = Some(Err(io::Error::other(error))),
    }
    self.ready.notify_one();
  }

  fn next(&self, cancellation: &CaptureCancellation) -> io::Result<Option<Frame>> {
    let mut events = self.events.lock().unwrap();
    loop {
      if let Some(finished) = events.finished.take() {
        return finished.map(|()| None);
      }
      if cancellation.is_cancelled() {
        return Ok(None);
      }
      if let Some(frame) = events.latest.take() {
        return Ok(Some(frame));
      }
      // Encoder failure can cancel capture without a ScreenCaptureKit callback.
      (events, _) = self
        .ready
        .wait_timeout(events, Duration::from_millis(50))
        .unwrap();
    }
  }
}

impl CaptureSource for &Frames {
  fn run<W: Write + Send>(self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let cancellation = lifetime.cancellation();
    while let Some(Frame {
      size: [width, height],
      pixels,
    }) = self.next(&cancellation)?
    {
      write!(output, "P6\n{width} {height}\n255\n")?;
      output.write_all(&pixels)?;
      output.flush()?;
    }
    Ok(())
  }
}

enum WindowStream<'a> {
  Raw,
  WithMetadata(&'a str),
  Video(&'a str, VideoRequest),
}

fn stream_window(
  id: &str,
  lease: Option<&str>,
  stream: WindowStream<'_>,
  input: &mut BufReader<UnixStream>,
  output: &mut Response,
) -> Result<()> {
  let identity = window_identity(id)?;
  let capture_options = match &stream {
    WindowStream::Video(_, request) => Some(CaptureOptions::new(request.options.max_fps)?),
    _ => None,
  };
  let window = list_windows()?
    .into_iter()
    .find(|window| window.id == identity.id && window.pid == identity.pid)
    .ok_or("the selected macOS window is no longer available")?;

  let frames = Arc::new(Frames::default());
  let publishing = Arc::clone(&frames);
  let session = Arc::new(CaptureSession::new(identity));
  let lifecycle = Arc::clone(&session);
  let independent = lease.is_some();
  let capture = run_on_main(move |main_thread| {
    let callback = move |event| {
      if matches!(event, Event::Frame { .. }) {
        lifecycle.received_frame();
      } else {
        lifecycle.end();
      }
      publishing.publish(event);
    };
    match (independent, capture_options) {
      (true, _) => Capture::start_session_window(&window, callback),
      (false, Some(options)) => Capture::start_window_with_options(&window, options, callback),
      (false, None) => Capture::start_window(&window, callback),
    }
    .map(|capture| MainThreadBound::new(capture, main_thread))
  })?;
  let _lease = CAPTURES.start(lease, Arc::clone(&session))?;
  let capture_input = input.get_ref().try_clone()?;
  let (notify, closed) = mpsc::channel();
  let lifetime = CaptureLifetime::from_receiver(closed);
  let result = thread::scope(|scope| {
    let disconnected = Arc::clone(&frames);
    let lifecycle = Arc::clone(&session);
    let cancellation = lifetime.cancellation();
    scope.spawn(move || {
      // A capture channel carries no input. EOF closes it even if the
      // selected window is idle and ScreenCaptureKit has no frames to send.
      let _ = input.read_exact(&mut [0]);
      lifecycle.end();
      cancellation.cancel();
      disconnected.publish(Event::Cancelled);
      drop(notify);
    });
    let result = match stream {
      WindowStream::Raw => frames.as_ref().run(output, &lifetime),
      WindowStream::WithMetadata(driver) => capture_started(
        frames.as_ref(),
        driver,
        CaptureFormat::Ppm,
        output,
        &lifetime,
      ),
      WindowStream::Video(driver, request) => capture_video_started(
        frames.as_ref(),
        driver,
        CaptureFormat::Ppm,
        request,
        output,
        &lifetime,
      ),
    };
    session.end();
    let _ = capture_input.shutdown(Shutdown::Read);
    result
  });
  drop(capture);
  result
}

fn forward_input(
  id: &str,
  lease: Option<&str>,
  input: &mut BufReader<UnixStream>,
  output: &mut impl Write,
  synchronized: bool,
) -> Result<()> {
  static INPUT_OPEN: AtomicBool = AtomicBool::new(false);
  struct ReleaseInput;
  impl Drop for ReleaseInput {
    fn drop(&mut self) {
      INPUT_OPEN.store(false, Ordering::Release);
    }
  }
  let _legacy_input = if lease.is_none() {
    if INPUT_OPEN.swap(true, Ordering::AcqRel) {
      return Err("a macOS window input channel is already open".into());
    }
    Some(ReleaseInput)
  } else {
    None
  };
  let identity = window_identity(id)?;
  let input_lease = CAPTURES.attach(lease, identity, input.get_ref())?;
  let mut control = WindowInput::open_with_delivery(identity, synchronized)?;
  if input_lease.ended() {
    return Err("the active macOS window capture has ended".into());
  }
  output.write_all(b"ready\n")?;
  output.flush()?;
  let mut line = String::new();
  loop {
    line.clear();
    let count = input.by_ref().take(129).read_line(&mut line)?;
    if count == 0 || input_lease.ended() {
      control.reset()?;
      return Ok(());
    }
    if count > 128 || !line.ends_with('\n') {
      return Err("input command exceeds 128 bytes or is incomplete".into());
    }
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    match fields.as_slice() {
      ["sync"] if synchronized => {
        output.write_all(b"ok\n")?;
        output.flush()?;
      }
      ["move", x, y, width, height] => {
        control.move_pointer(x.parse()?, y.parse()?, [width.parse()?, height.parse()?])?;
      }
      ["button", code, pressed @ ("0" | "1")] => control.button(code.parse()?, *pressed == "1")?,
      ["key", name, pressed @ ("0" | "1")] => control.key(name, *pressed == "1")?,
      ["reset"] => control.reset()?,
      _ => return Err("unknown macOS input command".into()),
    }
  }
}
