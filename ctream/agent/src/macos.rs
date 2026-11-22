mod socket;

use self::socket::{Listener, Response};
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
};
use ctream_driver::Result;
use ctream_driver_capture::macos::screen_capture_kit::{self, Capture, Event};
use ctream_driver_input::macos::{WindowIdentity, WindowInput, has_access};
use ctream_proto::{
  CaptureFormat, CaptureStarted, Display, PING_HELLO, PING_TOKEN, Session, Target, Window, WindowId,
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

pub fn run() -> Result<()> {
  let arguments: Vec<String> = std::env::args().skip(1).collect();
  if arguments == ["serve"] {
    serve()
  } else {
    parse(&arguments)?;
    socket::relay(&arguments)?;
    Ok(())
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
      stream_window(id, Some(selection.id), input, output)
    }
    Request::Target(Target::MacosDesktop, TargetOperation::CaptureWindow(id)) => {
      stream_window(id, None, input, output)
    }
    Request::Target(Target::MacosDesktop, TargetOperation::InputWindow(id)) => {
      forward_input(id, input, output)
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

static ACTIVE_CAPTURE: Mutex<Option<Arc<CaptureSession>>> = Mutex::new(None);

struct CaptureSession {
  identity: WindowIdentity,
  ended: AtomicBool,
  input: Mutex<Option<UnixStream>>,
}

impl CaptureSession {
  fn new(identity: WindowIdentity) -> Self {
    Self {
      identity,
      ended: AtomicBool::new(false),
      input: Mutex::new(None),
    }
  }

  fn end(&self) {
    if !self.ended.swap(true, Ordering::AcqRel)
      && let Some(stream) = self.input.lock().unwrap().as_ref()
    {
      let _ = stream.shutdown(Shutdown::Read);
    }
  }

  fn attach(self: Arc<Self>, identity: WindowIdentity, stream: &UnixStream) -> Result<InputLease> {
    if self.identity != identity {
      return Err("macOS window input must match the active window capture".into());
    }
    let mut input = self.input.lock().unwrap();
    if self.ended.load(Ordering::Acquire) {
      return Err("the active macOS window capture has ended".into());
    }
    if input.is_some() {
      return Err("a macOS window input channel is already open".into());
    }
    *input = Some(stream.try_clone()?);
    drop(input);
    Ok(InputLease(self))
  }
}

struct CaptureLease(Arc<CaptureSession>);

impl CaptureLease {
  fn start(session: Arc<CaptureSession>) -> Result<Self> {
    let mut active = ACTIVE_CAPTURE.lock().unwrap();
    if active.is_some() {
      return Err("a macOS window capture is already open".into());
    }
    *active = Some(Arc::clone(&session));
    Ok(Self(session))
  }
}

impl Drop for CaptureLease {
  fn drop(&mut self) {
    self.0.end();
    ACTIVE_CAPTURE.lock().unwrap().take();
  }
}

struct InputLease(Arc<CaptureSession>);

impl Drop for InputLease {
  fn drop(&mut self) {
    self.0.input.lock().unwrap().take();
  }
}

struct Frame {
  size: [usize; 2],
  pixels: Vec<u8>,
}

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

  fn next(&self) -> io::Result<Option<Frame>> {
    let mut events = self.events.lock().unwrap();
    loop {
      if let Some(finished) = events.finished.take() {
        return finished.map(|()| None);
      }
      if let Some(frame) = events.latest.take() {
        return Ok(Some(frame));
      }
      events = self.ready.wait(events).unwrap();
    }
  }
}

fn stream_window(
  id: &str,
  mut driver: Option<&str>,
  input: &mut BufReader<UnixStream>,
  output: &mut Response,
) -> Result<()> {
  let identity = window_identity(id)?;
  let window = list_windows()?
    .into_iter()
    .find(|window| window.id == identity.id && window.pid == identity.pid)
    .ok_or("the selected macOS window is no longer available")?;

  let frames = Arc::new(Frames::default());
  let publishing = Arc::clone(&frames);
  let session = Arc::new(CaptureSession::new(identity));
  let lifecycle = Arc::clone(&session);
  let capture = run_on_main(move |main_thread| {
    Capture::start_window(&window, move |event| {
      if !matches!(event, Event::Frame { .. }) {
        lifecycle.end();
      }
      publishing.publish(event);
    })
    .map(|capture| MainThreadBound::new(capture, main_thread))
  })?;
  let _lease = CaptureLease::start(Arc::clone(&session))?;
  let cancellation = input.get_ref().try_clone()?;
  let result = thread::scope(|scope| {
    let disconnected = Arc::clone(&frames);
    let lifecycle = Arc::clone(&session);
    scope.spawn(move || {
      // A capture channel carries no input. EOF closes it even if the
      // selected window is idle and ScreenCaptureKit has no frames to send.
      let _ = input.read_exact(&mut [0]);
      lifecycle.end();
      disconnected.publish(Event::Cancelled);
    });
    let result = (|| {
      while let Some(Frame {
        size: [width, height],
        pixels,
      }) = frames.next()?
      {
        if let Some(id) = driver.take() {
          serde_json::to_writer(
            &mut *output,
            &CaptureStarted {
              driver: id.into(),
              format: CaptureFormat::Ppm,
            },
          )?;
          output.write_all(b"\n")?;
        }
        write!(output, "P6\n{width} {height}\n255\n")?;
        output.write_all(&pixels)?;
        output.flush()?;
      }
      Ok(())
    })();
    session.end();
    let _ = cancellation.shutdown(Shutdown::Read);
    result
  });
  drop(capture);
  result
}

fn forward_input(
  id: &str,
  input: &mut BufReader<UnixStream>,
  output: &mut impl Write,
) -> Result<()> {
  static INPUT_OPEN: AtomicBool = AtomicBool::new(false);
  if INPUT_OPEN.swap(true, Ordering::AcqRel) {
    return Err("a macOS window input channel is already open".into());
  }
  struct ReleaseInput;
  impl Drop for ReleaseInput {
    fn drop(&mut self) {
      INPUT_OPEN.store(false, Ordering::Release);
    }
  }
  let _release = ReleaseInput;
  let identity = window_identity(id)?;
  let session = ACTIVE_CAPTURE
    .lock()
    .unwrap()
    .as_ref()
    .cloned()
    .ok_or("macOS window input requires an active window capture")?;
  let _input = Arc::clone(&session).attach(identity, input.get_ref())?;
  let mut control = WindowInput::open(identity)?;
  if session.ended.load(Ordering::Acquire) {
    return Err("the active macOS window capture has ended".into());
  }
  output.write_all(b"ready\n")?;
  output.flush()?;
  let mut line = String::new();
  loop {
    line.clear();
    let count = input.by_ref().take(129).read_line(&mut line)?;
    if count == 0 || session.ended.load(Ordering::Acquire) {
      control.reset()?;
      return Ok(());
    }
    if count > 128 || !line.ends_with('\n') {
      return Err("input command exceeds 128 bytes or is incomplete".into());
    }
    let fields: Vec<_> = line.split_ascii_whitespace().collect();
    match fields.as_slice() {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn capture_end_releases_input_for_the_selected_window() {
    let identity = WindowIdentity { id: 7, pid: 10 };
    let session = Arc::new(CaptureSession::new(identity));
    let (peer, mut input) = UnixStream::pair().unwrap();
    assert!(
      Arc::clone(&session)
        .attach(WindowIdentity { id: 7, pid: 11 }, &input)
        .is_err()
    );
    let lease = Arc::clone(&session).attach(identity, &input).unwrap();
    assert!(Arc::clone(&session).attach(identity, &input).is_err());
    session.end();
    // Keep the client open and prove that stopping capture ends the receiver.
    input
      .set_read_timeout(Some(Duration::from_secs(1)))
      .unwrap();
    assert_eq!(input.read(&mut [0]).unwrap(), 0);
    drop(peer);
    drop(lease);
    assert!(session.attach(identity, &input).is_err());
  }
}
