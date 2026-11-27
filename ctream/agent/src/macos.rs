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
  native_input::{self, Gate, Mode},
  protocol::{Request, TargetOperation, parse},
  runtime::{capture_started, capture_video_started, input_capabilities, video_capabilities},
};
use ctream_driver::{CaptureLifetime, CaptureOptions, CaptureSource, Frame, Result};
use ctream_driver_capture::macos::screen_capture_kit::{self, Capture, Event};
use ctream_driver_input::macos::{WindowIdentity, WindowInput, has_access};
use ctream_proto::{
  CaptureFormat, Display, PING_HELLO, PING_TOKEN, Session, Target, VideoRequest, Window, WindowId,
};
use dispatch2::{DispatchQueue, MainThreadBound, run_on_main};
use std::{
  ffi::c_void,
  io::{self, BufReader, Read, Write},
  net::Shutdown,
  os::unix::net::UnixStream,
  ptr,
  sync::{Arc, LazyLock, mpsc},
  thread,
  time::Duration,
};

pub fn run(arguments: &[String]) -> Result<()> {
  if arguments == ["serve"] {
    return serve();
  }
  // SSH sessions submit input to the desktop daemon. ScreenCaptureKit and
  // native delivery run in the permission-bearing GUI worker.
  match parse(arguments)? {
    Request::ViewerLane(target, window, lane) => crate::viewer::run(target, window, lane),
    Request::LiveRoom {
      turns,
      authoritative,
      target,
      agent_id,
      window,
    } => crate::live_room(target, agent_id, window, turns, authoritative),
    Request::AttributedSession {
      protocol,
      target,
      agent_id,
      window,
    } => crate::attributed::serve(target, agent_id, window, protocol),
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
  delegation: Option<&str>,
  input: &mut BufReader<UnixStream>,
  output: &mut Response,
) -> Result<()> {
  match parse(arguments)? {
    Request::Ping => ping(input, output),
    Request::InputCapabilities => input_capabilities(output),
    Request::ViewerInput {
      target: Target::MacosDesktop,
      label,
      window: Some(id),
      receipts,
    } => forward_input(
      id,
      None,
      input,
      output,
      Mode::Viewer { receipts },
      Some(label),
      delegation,
    ),
    Request::InputSync(Target::MacosDesktop, Some(id)) => forward_input(
      id,
      None,
      input,
      output,
      Mode::Synchronized,
      None,
      delegation,
    ),
    Request::ViewerCapture {
      window,
      lease,
      driver,
      video,
    } => {
      let selection = capture::select(Target::MacosDesktop, driver, true)?;
      let format = match video {
        Some(request) => WindowStream::Video(selection.id, request),
        None => WindowStream::WithMetadata(selection.id),
      };
      stream_window(window, Some(lease), format, input, output)
    }
    Request::LeasedViewerInput {
      window,
      lease,
      label,
    } => forward_input(
      window,
      Some(lease),
      input,
      output,
      Mode::Viewer { receipts: true },
      Some(label),
      delegation,
    ),
    Request::SessionCapture { window, lease } => {
      stream_window(window, Some(lease), WindowStream::Raw, input, output)
    }
    Request::SessionInput { window, lease } => forward_input(
      window,
      Some(lease),
      input,
      output,
      Mode::Synchronized,
      None,
      delegation,
    ),
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
      forward_input(id, None, input, output, Mode::Legacy, None, delegation)
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

// ScreenCaptureKit handles stay in the permissioned GUI host. A subscriber owns
// a separate input lease even when another subscriber uses the same stream.
type WindowSources = capture::pool::Pool<(u32, i32, u16)>;
static SOURCES: LazyLock<WindowSources> = LazyLock::new(WindowSources::default);

struct WindowFrames {
  subscriber: capture::pool::Subscription,
  session: Arc<CaptureSession>,
}

impl CaptureSource for WindowFrames {
  fn run<W: Write + Send>(mut self, output: &mut W, lifetime: &CaptureLifetime) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()?;
    let cancellation = lifetime.cancellation();
    runtime.block_on(async {
      while !cancellation.is_cancelled() {
        match tokio::time::timeout(Duration::from_millis(50), self.subscriber.next()).await {
          Err(_) => continue,
          Ok(Ok(None)) => break,
          Ok(Err(error)) => return Err(error),
          Ok(Ok(Some(frame))) => {
            let [width, height] = frame.size;
            self.session.received_frame();
            write!(output, "P6\n{width} {height}\n255\n")?;
            output.write_all(&frame.pixels)?;
            output.flush()?;
          }
        }
      }
      Ok::<(), io::Error>(())
    })?;
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

  let subscriber = SOURCES.subscribe(
    (
      identity.id,
      identity.pid,
      capture_options.map_or(screen_capture_kit::DEFAULT_MAX_FPS, CaptureOptions::max_fps),
    ),
    move |publishing, lifetime| {
      let (end, finished) = mpsc::channel();
      let _capture = run_on_main(move |main_thread| {
        let callback = move |event| match event {
          Event::Frame { size, pixels } => {
            publishing.publish(Frame { size, pixels });
          }
          Event::Cancelled | Event::Finished => {
            publishing.end();
            let _ = end.send(Ok(()));
          }
          Event::Failed(error) => {
            publishing.end();
            let _ = end.send(Err(io::Error::other(error)));
          }
        };
        match capture_options {
          Some(options) => Capture::start_session_window_with_options(&window, options, callback),
          None => Capture::start_session_window(&window, callback),
        }
        .map(|capture| MainThreadBound::new(capture, main_thread))
      })?;
      while !lifetime.cancelled(Duration::from_millis(20)) {
        match finished.try_recv() {
          Ok(result) => return result,
          Err(mpsc::TryRecvError::Disconnected) => break,
          Err(mpsc::TryRecvError::Empty) => {}
        }
      }
      Ok(())
    },
  )?;
  let session = Arc::new(CaptureSession::new(identity, subscriber.status()));
  let _lease = CAPTURES.start(lease, Arc::clone(&session))?;
  let frames = WindowFrames {
    subscriber,
    session: session.clone(),
  };
  let capture_input = input.get_ref().try_clone()?;
  let (notify, closed) = mpsc::channel();
  let lifetime = CaptureLifetime::from_receiver(closed);
  thread::scope(|scope| {
    let lifecycle = Arc::clone(&session);
    let cancellation = lifetime.cancellation();
    scope.spawn(move || {
      // A capture channel carries no input. EOF closes it even if the
      // selected window is idle and ScreenCaptureKit has no frames to send.
      let _ = input.read_exact(&mut [0]);
      lifecycle.end();
      cancellation.cancel();
      drop(notify);
    });
    let result = match stream {
      WindowStream::Raw => frames.run(output, &lifetime),
      WindowStream::WithMetadata(driver) => {
        capture_started(frames, driver, CaptureFormat::Ppm, output, &lifetime)
      }
      WindowStream::Video(driver, request) => capture_video_started(
        frames,
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
  })
}

fn forward_input(
  id: &str,
  lease: Option<&str>,
  input: &mut BufReader<UnixStream>,
  output: &mut impl Write,
  mode: Mode,
  label: Option<&str>,
  delegation: Option<&str>,
) -> Result<()> {
  let Some(key) = delegation else {
    let authority = native_input::controller(Target::MacosDesktop, Some(id), label, None)?;
    return Ok(native_input::serve_controller(
      mode, authority, None, lease, input, output,
    )?);
  };
  let mut gate = Gate::delegated(Target::MacosDesktop, Some(id), key)?;
  let identity = window_identity(id)?;
  let input_lease = CAPTURES.attach(lease, identity, input.get_ref())?;
  let mut control = WindowInput::open_with_delivery(identity, mode != Mode::Legacy)?;
  if input_lease.ended() {
    return Err("the active macOS window capture has ended".into());
  }
  output.write_all(b"ready\n")?;
  output.flush()?;
  let control_socket = input.get_ref().try_clone()?;
  let input = std::mem::replace(input, BufReader::new(control_socket.try_clone()?));
  let received = native_input::receive(mode, &mut gate, input, output, |commands| {
    if input_lease.ended() {
      return Err(io::Error::other(
        "the active macOS window capture has ended",
      ));
    }
    for command in commands {
      control.dispatch(command)?;
    }
    Ok(())
  });
  let _ = control_socket.shutdown(Shutdown::Read);
  let released = control.reset();
  drop(control);
  let finished = gate.finish();
  received.and(released).and(finished)?;
  Ok(())
}
