//! Typed session operations, transports, and media handling for ctream clients.

pub mod automation;
pub mod geometry;
pub mod input;
mod latency;
pub mod media;
pub mod options;
pub mod transport;

use crate::{
  input::{Input, InputFormat},
  media::{Capture, FrameFormat},
  transport::{Channel, Transport},
};
use ctream_proto::{
  Action, ActionResult, BOUNDS_VERSION, CAPTURE_CATALOG_VERSION, CAPTURE_DRIVER_VERSION,
  CaptureDriverDescriptor, CaptureDriverInfo, DISPLAY_VERSION, Element, LATENCY_VERSION,
  MACOS_WINDOW_VERSION, Session as DisplaySession, Target, VIDEO_STREAM_VERSION, VideoCapabilities,
  VideoCodec, VideoOptions, VideoRequest, WAYLAND_WINDOW_VERSION, WINDOWS_DESKTOP_VERSION,
  WINDOWS_DISPLAY_VERSION, WINDOWS_WINDOW_VERSION, Window, WindowId, X11_FRAME_SIZE,
  X11_WINDOW_INPUT_VERSION, X11_WINDOW_VERSION, valid_capture_driver, valid_window_id,
};
pub use latency::LatencyProbe;
use serde::de::DeserializeOwned;
use std::{
  io::{self, Read, Write},
  sync::Arc,
};

/// A JSON operation binds the wire request to the type of its reply.
pub trait Query {
  type Reply: DeserializeOwned;

  fn arguments(&self) -> io::Result<Vec<String>>;

  fn payload(&self) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
  }
}

pub struct Discover;

impl Query for Discover {
  type Reply = Vec<DisplaySession>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    DiscoverWithVersion(WINDOWS_DISPLAY_VERSION).arguments()
  }
}

struct DiscoverWithVersion(&'static str);

impl Query for DiscoverWithVersion {
  type Reply = Vec<DisplaySession>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![self.0.into(), "discover".into()])
  }
}

pub struct Windows(pub Target);

pub struct CaptureDrivers(pub Target);

pub struct CaptureCatalog;

pub struct VideoCodecs;

impl Query for VideoCodecs {
  type Reply = VideoCapabilities;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![VIDEO_STREAM_VERSION.into(), "video-codecs".into()])
  }
}

impl Query for CaptureCatalog {
  type Reply = Vec<CaptureDriverDescriptor>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    CaptureCatalogWithVersion(CAPTURE_CATALOG_VERSION).arguments()
  }
}

struct CaptureCatalogWithVersion(&'static str);

impl Query for CaptureCatalogWithVersion {
  type Reply = Vec<CaptureDriverDescriptor>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![self.0.into(), "capture-drivers".into()])
  }
}

impl Query for CaptureDrivers {
  type Reply = Vec<CaptureDriverInfo>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![
      capture_driver_version(self.0).into(),
      "capture-drivers".into(),
      self.0.to_string(),
    ])
  }
}

impl Query for Windows {
  type Reply = Vec<Window>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![
      window_version(self.0).into(),
      "windows".into(),
      self.0.to_string(),
    ])
  }
}

struct Elements<'a> {
  target: Target,
  window: &'a str,
  version: &'static str,
}

impl Query for Elements<'_> {
  type Reply = Vec<Element>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    window_arguments(self.version, "elements", self.target, self.window)
  }
}

struct ApplyAction<'a> {
  target: Target,
  window: &'a str,
  action: Action,
}

impl Query for ApplyAction<'_> {
  type Reply = ActionResult;

  fn arguments(&self) -> io::Result<Vec<String>> {
    window_arguments(
      window_version(self.target),
      "action",
      self.target,
      self.window,
    )
  }

  fn payload(&self) -> io::Result<Option<Vec<u8>>> {
    let data = serde_json::to_vec(&self.action)?;
    if data.len() > 65536 {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "action exceeds the request limit",
      ));
    }
    Ok(Some(data))
  }
}

pub struct Client<T> {
  transport: Arc<T>,
}

impl<T> Clone for Client<T> {
  fn clone(&self) -> Self {
    Self {
      transport: Arc::clone(&self.transport),
    }
  }
}

impl<T: Transport> Client<T> {
  pub fn new(transport: T) -> Self {
    Self {
      transport: Arc::new(transport),
    }
  }

  pub fn query<Q: Query>(&self, query: Q) -> io::Result<Q::Reply> {
    let arguments = query.arguments()?;
    let payload = query.payload()?;
    let Channel {
      reader,
      mut writer,
      cancellation,
    } = self.transport.open(&arguments)?;
    let result = (|| {
      if let Some(payload) = payload {
        writer.write_all(&payload)?;
      }
      drop(writer);
      let mut data = Vec::new();
      const LIMIT: usize = 1024 * 1024;
      reader.take((LIMIT + 1) as u64).read_to_end(&mut data)?;
      if data.len() > LIMIT {
        return Err(io::Error::other("ctream agent response exceeds 1 MiB"));
      }
      serde_json::from_slice(&data).map_err(io::Error::other)
    })();
    cancellation.cancel();
    result
  }

  pub fn discover(&self) -> io::Result<Vec<DisplaySession>> {
    self
      .query(DiscoverWithVersion(MACOS_WINDOW_VERSION))
      .or_else(|error| {
        self
          .query(Discover)
          .or_else(|_| self.query(DiscoverWithVersion(DISPLAY_VERSION)))
          .map_err(|_| error)
      })
  }

  pub fn windows(&self, target: Target) -> io::Result<Vec<Window>> {
    self.query(Windows(target))
  }

  pub fn capture_drivers(&self, target: Target) -> io::Result<Vec<CaptureDriverInfo>> {
    self.query(CaptureDrivers(target))
  }

  /// List compiled providers without selecting or opening a display session.
  pub fn capture_catalog(&self) -> io::Result<Vec<CaptureDriverDescriptor>> {
    self
      .query(CaptureCatalogWithVersion(MACOS_WINDOW_VERSION))
      .or_else(|error| self.query(CaptureCatalog).map_err(|_| error))
  }

  pub fn video_capabilities(&self) -> io::Result<VideoCapabilities> {
    self.query(VideoCodecs)
  }

  /// Open a dedicated round-trip probe. The first sample waits for the agent's
  /// handshake so the measured duration excludes channel startup.
  pub fn latency_probe(&self) -> io::Result<LatencyProbe<T::Reader, T::Writer>> {
    let channel = self
      .transport
      .open(&[LATENCY_VERSION.into(), "ping".into()])?;
    Ok(LatencyProbe::new(channel))
  }

  pub fn session(&self, target: Target, window: Option<Window>) -> Session<T> {
    Session {
      client: self.clone(),
      target,
      window,
    }
  }
}

/// A target selection shared by capture, input, and semantic operations.
/// Each operation owns an independently cancellable channel on retained agents.
pub struct Session<T> {
  client: Client<T>,
  target: Target,
  window: Option<Window>,
}

impl<T> Clone for Session<T> {
  fn clone(&self) -> Self {
    Self {
      client: self.client.clone(),
      target: self.target,
      window: self.window.clone(),
    }
  }
}

impl<T: Transport> Session<T> {
  /// Negotiate compressed video, retaining raw capture for agents supporting
  /// the earlier driver-selection contract. Pre-selection peers use `capture`.
  pub fn capture_with_options(
    &self,
    driver: &str,
    options: VideoOptions,
  ) -> io::Result<Capture<T::Reader, T::Writer>> {
    self.require_window_for_desktop()?;
    if !valid_capture_driver(driver) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid capture driver identifier",
      ));
    }
    options
      .validate()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if let Some(window) = &self.window {
      validate_window_id(self.target, &window.id)?;
    }
    let capabilities = match self.client.video_capabilities() {
      Ok(capabilities) => capabilities,
      Err(_) => return self.capture_with_driver(driver),
    };
    if capabilities.codecs.len() > 2
      || capabilities.codecs.contains(&VideoCodec::Auto)
      || (capabilities.codecs.len() == 2 && capabilities.codecs[0] == capabilities.codecs[1])
    {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "agent reported invalid video codecs",
      ));
    }
    let local = ctream_driver_codec::ffmpeg::available_decoders()?;
    let request = VideoRequest {
      options,
      codecs: common_video_codecs(options.codec, &capabilities.codecs, &local),
    };
    if request.codecs.is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "capture host and viewer have no compatible video codec",
      ));
    }
    request
      .validate()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut arguments = vec![
      VIDEO_STREAM_VERSION.into(),
      "capture-stream".into(),
      self.target.to_string(),
      driver.into(),
      serde_json::to_string(&request)?,
    ];
    if let Some(window) = &self.window {
      arguments.push(window.id.clone());
    }
    Capture::started_video(self.client.transport.open(&arguments)?, driver, &request)
  }

  pub fn capture_with_driver(&self, driver: &str) -> io::Result<Capture<T::Reader, T::Writer>> {
    self.require_window_for_desktop()?;
    if !valid_capture_driver(driver) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid capture driver identifier",
      ));
    }
    let mut arguments = vec![
      capture_driver_version(self.target).into(),
      "capture-with-driver".into(),
      self.target.to_string(),
      driver.into(),
    ];
    if let Some(window) = &self.window {
      validate_window_id(self.target, &window.id)?;
      arguments.push(window.id.clone());
    }
    Capture::started(self.client.transport.open(&arguments)?, driver)
  }

  pub fn supports_input(&self) -> bool {
    match self.target {
      Target::WindowsDesktop | Target::MacosDesktop => self.window.is_some(),
      Target::X11(_) | Target::Windows(_) => true,
      Target::Wayland(_) => self.window.is_none(),
    }
  }

  pub fn supports_semantics(&self) -> bool {
    self.window.is_some() && matches!(self.target, Target::Wayland(_))
  }

  pub fn capture(&self) -> io::Result<Capture<T::Reader, T::Writer>> {
    let (arguments, format) = if let Some(window) = &self.window {
      (
        window_arguments(
          window_version(self.target),
          "capture-window",
          self.target,
          &window.id,
        )?,
        FrameFormat::Ppm,
      )
    } else {
      let format = match self.target {
        Target::X11(_) => FrameFormat::FixedRgb {
          size: X11_FRAME_SIZE,
        },
        Target::Wayland(_) | Target::Windows(_) => FrameFormat::Ppm,
        Target::WindowsDesktop | Target::MacosDesktop => {
          return Err(desktop_window_required(self.target));
        }
      };
      (
        vec![
          display_version(self.target).into(),
          "capture".into(),
          self.target.to_string(),
        ],
        format,
      )
    };
    Ok(Capture::new(
      self.client.transport.open(&arguments)?,
      format,
    ))
  }

  pub fn input(&self) -> io::Result<Input<T::Reader, T::Writer>> {
    self.require_window_for_desktop()?;
    if !self.supports_input() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "pointer and keyboard input require a display or a supported window",
      ));
    }
    let format = match self.target {
      Target::X11(_) if self.window.is_some() => InputFormat::X11Window,
      Target::X11(_) => InputFormat::X11,
      Target::Wayland(_) => InputFormat::Wayland,
      Target::Windows(_) | Target::WindowsDesktop => InputFormat::Windows,
      Target::MacosDesktop => InputFormat::Macos,
    };
    let arguments = if let Some(window) = &self.window {
      let version = if matches!(format, InputFormat::X11Window) {
        WindowId::x11_xid(&window.id).ok_or_else(|| {
          io::Error::new(io::ErrorKind::InvalidInput, "invalid X11 window identifier")
        })?;
        X11_WINDOW_INPUT_VERSION
      } else {
        window_version(self.target)
      };
      window_arguments(version, "input-window", self.target, &window.id)?
    } else {
      vec![
        display_version(self.target).into(),
        "input".into(),
        self.target.to_string(),
      ]
    };
    Input::open(self.client.transport.open(&arguments)?, format)
  }

  fn semantic_window(&self) -> io::Result<&str> {
    if !self.supports_semantics() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "semantic actions require a supported Wayland window",
      ));
    }
    Ok(&self.window.as_ref().expect("window capability").id)
  }

  fn require_window_for_desktop(&self) -> io::Result<()> {
    if self.target.window_only() && self.window.is_none() {
      return Err(desktop_window_required(self.target));
    }
    Ok(())
  }

  pub fn elements(&self) -> io::Result<Vec<Element>> {
    let window = self.semantic_window()?;
    self
      .client
      .query(Elements {
        target: self.target,
        window,
        version: BOUNDS_VERSION,
      })
      .or_else(|error| {
        self
          .client
          .query(Elements {
            target: self.target,
            window,
            version: WAYLAND_WINDOW_VERSION,
          })
          .map_err(|_| error)
      })
  }

  pub fn action(&self, action: Action) -> io::Result<ActionResult> {
    self.client.query(ApplyAction {
      target: self.target,
      window: self.semantic_window()?,
      action,
    })
  }
}

fn common_video_codecs(
  preferred: VideoCodec,
  remote: &[VideoCodec],
  local: &[VideoCodec],
) -> Vec<VideoCodec> {
  let preferences: &[VideoCodec] = match preferred {
    VideoCodec::Auto | VideoCodec::Av1 => &[VideoCodec::Av1, VideoCodec::H264],
    VideoCodec::H264 => &[VideoCodec::H264],
  };
  preferences
    .iter()
    .copied()
    .filter(|codec| remote.contains(codec) && local.contains(codec))
    .collect()
}

fn display_version(target: Target) -> &'static str {
  if matches!(target, Target::Windows(_)) {
    WINDOWS_DISPLAY_VERSION
  } else {
    DISPLAY_VERSION
  }
}

fn capture_driver_version(target: Target) -> &'static str {
  match target {
    Target::WindowsDesktop => WINDOWS_DESKTOP_VERSION,
    Target::MacosDesktop => MACOS_WINDOW_VERSION,
    _ => CAPTURE_DRIVER_VERSION,
  }
}

fn window_version(target: Target) -> &'static str {
  match target {
    Target::X11(_) => X11_WINDOW_VERSION,
    Target::Wayland(_) => WAYLAND_WINDOW_VERSION,
    Target::Windows(_) => WINDOWS_WINDOW_VERSION,
    Target::WindowsDesktop => WINDOWS_DESKTOP_VERSION,
    Target::MacosDesktop => MACOS_WINDOW_VERSION,
  }
}

fn desktop_window_required(target: Target) -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidInput,
    if target == Target::MacosDesktop {
      "macOS desktop target requires a window ID"
    } else {
      "Windows desktop target requires a window ID"
    },
  )
}

fn window_arguments(
  version: &str,
  operation: &str,
  target: Target,
  id: &str,
) -> io::Result<Vec<String>> {
  validate_window_id(target, id)?;
  Ok(vec![
    version.into(),
    operation.into(),
    target.to_string(),
    id.into(),
  ])
}

fn validate_window_id(target: Target, id: &str) -> io::Result<()> {
  let valid = if target == Target::MacosDesktop {
    WindowId::macos_parts(id).is_some()
  } else {
    valid_window_id(id)
  };
  if !valid {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "invalid window identifier",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::transport::Cancellation;
  use std::{collections::VecDeque, io::Cursor, sync::Mutex};

  #[derive(Default)]
  struct Observed {
    requests: Mutex<Vec<Vec<String>>>,
    payloads: Mutex<Vec<u8>>,
  }

  struct Mock {
    observed: Arc<Observed>,
    responses: Mutex<VecDeque<Vec<u8>>>,
  }

  struct Writer(Arc<Observed>);

  impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self.0.payloads.lock().unwrap().extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
      Ok(())
    }
  }

  impl Transport for Mock {
    type Reader = Cursor<Vec<u8>>;
    type Writer = Writer;

    fn open(&self, arguments: &[String]) -> io::Result<Channel<Self::Reader, Self::Writer>> {
      self
        .observed
        .requests
        .lock()
        .unwrap()
        .push(arguments.to_vec());
      let response = self
        .responses
        .lock()
        .unwrap()
        .pop_front()
        .ok_or_else(|| io::Error::other("unexpected request"))?;
      Ok(Channel {
        reader: Cursor::new(response),
        writer: Writer(Arc::clone(&self.observed)),
        cancellation: Cancellation::default(),
      })
    }
  }

  fn client(responses: &[&str]) -> (Client<Mock>, Arc<Observed>) {
    let observed = Arc::new(Observed::default());
    (
      Client::new(Mock {
        observed: Arc::clone(&observed),
        responses: Mutex::new(
          responses
            .iter()
            .map(|data| data.as_bytes().to_vec())
            .collect(),
        ),
      }),
      observed,
    )
  }

  fn window() -> Window {
    Window {
      id: "window_1".into(),
      title: "Example".into(),
      app_id: "example".into(),
    }
  }

  #[test]
  fn latency_probe_reuses_one_channel_and_requires_the_handshake() {
    let (client, observed) = client(&["!??", "", "!x"]);
    let mut probe = client.latency_probe().unwrap();
    probe.sample().unwrap();
    probe.sample().unwrap();
    assert_eq!(*observed.requests.lock().unwrap(), [["1.0.10", "ping"]]);
    assert_eq!(*observed.payloads.lock().unwrap(), b"??");

    let mut old_agent = client.latency_probe().unwrap();
    assert_eq!(
      old_agent.sample().unwrap_err().kind(),
      io::ErrorKind::UnexpectedEof,
    );
    assert_eq!(*observed.payloads.lock().unwrap(), b"??");

    let mut broken = client.latency_probe().unwrap();
    assert_eq!(
      broken.sample().unwrap_err().kind(),
      io::ErrorKind::InvalidData,
    );
    assert_eq!(
      broken.sample().unwrap_err().kind(),
      io::ErrorKind::NotConnected,
    );
  }

  #[test]
  fn client_selects_retained_contracts_and_typed_replies() {
    let (client, observed) = client(&[
      "[]",
      r#"[{"id":"x11-1","title":"Example","app_id":"example"}]"#,
      "[]",
    ]);
    assert!(client.query(Discover).unwrap().is_empty());
    assert_eq!(client.windows(Target::X11(1)).unwrap()[0].id, "x11-1");
    assert!(
      client
        .clone()
        .windows(Target::Wayland(1))
        .unwrap()
        .is_empty()
    );
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![WINDOWS_DISPLAY_VERSION, "discover"],
        vec!["1.0.3", "windows", ":1"],
        vec!["1.0.2", "windows", "wayland-1"],
      ]
    );
  }

  #[test]
  fn discovery_falls_back_to_the_original_linux_contract() {
    let (client, observed) = client(&["unsupported contract", "unsupported contract", "[]"]);
    assert!(client.discover().unwrap().is_empty());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![MACOS_WINDOW_VERSION, "discover"],
        vec![WINDOWS_DISPLAY_VERSION, "discover"],
        vec![DISPLAY_VERSION, "discover"],
      ]
    );
  }

  #[test]
  fn codec_negotiation_prefers_av1_and_only_falls_back_to_a_shared_decoder() {
    use VideoCodec::{Auto, Av1, H264};
    for preference in [Auto, Av1] {
      assert_eq!(
        common_video_codecs(preference, &[H264, Av1], &[H264, Av1]),
        [Av1, H264]
      );
      assert_eq!(
        common_video_codecs(preference, &[Av1, H264], &[H264]),
        [H264]
      );
      assert_eq!(
        common_video_codecs(preference, &[H264], &[Av1, H264]),
        [H264]
      );
      assert!(common_video_codecs(preference, &[Av1], &[H264]).is_empty());
    }
    assert_eq!(
      common_video_codecs(H264, &[Av1, H264], &[Av1, H264]),
      [H264]
    );
    assert!(common_video_codecs(H264, &[Av1], &[Av1, H264]).is_empty());
  }

  #[test]
  fn old_agents_keep_negotiated_raw_capture_and_selected_driver() {
    use crate::media::{Frame, VideoSource};
    for driver in ["auto", "linux.x11.ffmpeg"] {
      let (client, observed) = client(&[
        "unsupported contract",
        concat!(
          r#"{"driver":"linux.x11.ffmpeg","format":{"type":"fixed_rgb","size":[1,1]}}"#,
          "\nxyz"
        ),
      ]);
      let mut capture = client
        .session(Target::X11(7), None)
        .capture_with_options(driver, VideoOptions::default())
        .unwrap();
      assert_eq!(capture.driver(), Some("linux.x11.ffmpeg"));
      assert_eq!(capture.video(), None);
      let mut frame = Frame::default();
      capture.read_frame(&mut frame).unwrap();
      assert_eq!(frame.pixels, b"xyz");
      assert_eq!(
        *observed.requests.lock().unwrap(),
        [
          vec![VIDEO_STREAM_VERSION, "video-codecs"],
          vec![CAPTURE_DRIVER_VERSION, "capture-with-driver", ":7", driver],
        ]
      );
    }
  }

  #[test]
  fn invalid_video_requests_fail_before_opening_a_channel() {
    let (client, observed) = client(&[]);
    let session = client.session(Target::X11(7), None);
    assert!(
      session
        .capture_with_options("bad driver", VideoOptions::default())
        .is_err()
    );
    assert!(
      session
        .capture_with_options(
          "auto",
          VideoOptions {
            max_fps: 0,
            ..VideoOptions::default()
          }
        )
        .is_err()
    );
    assert!(
      client
        .session(Target::WindowsDesktop, None)
        .capture_with_options("auto", VideoOptions::default())
        .is_err()
    );
    assert!(
      client
        .session(
          Target::X11(7),
          Some(Window {
            id: "bad/window".into(),
            ..window()
          })
        )
        .capture_with_options("auto", VideoOptions::default())
        .is_err()
    );
    assert!(observed.requests.lock().unwrap().is_empty());
  }

  #[test]
  fn invalid_video_capabilities_do_not_retry_raw_capture() {
    for capabilities in [r#"{"codecs":["auto"]}"#, r#"{"codecs":["av1","av1"]}"#] {
      let (client, observed) = client(&[capabilities]);
      let error = match client
        .session(Target::X11(7), None)
        .capture_with_options("auto", VideoOptions::default())
      {
        Ok(_) => panic!("invalid capabilities accepted"),
        Err(error) => error,
      };
      assert_eq!(error.kind(), io::ErrorKind::InvalidData);
      assert_eq!(
        *observed.requests.lock().unwrap(),
        [[VIDEO_STREAM_VERSION, "video-codecs"]]
      );
    }
  }

  #[test]
  fn x11_window_input_has_an_independent_handshake_and_version() {
    use crate::media::{Frame, VideoSource};

    let (client, observed) = client(&[
      r#"[{"id":"x11-abc","title":"Example","app_id":"example"}]"#,
      "P6\n2 1\n255\nabcdefP6\n1 2\n255\nghijkl",
      "ready\n",
      "800 600\n",
    ]);
    let target = Target::X11(7);
    let session = client.session(target, client.windows(target).unwrap().pop());
    assert!(session.supports_input());
    assert!(!session.supports_semantics());

    let mut capture = session.capture().unwrap();
    let mut frame = Frame::default();
    capture.read_frame(&mut frame).unwrap();
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [1, 2]);
    let mut input = session.input().unwrap();
    assert_eq!(input.dimensions(), None);
    input.move_pointer(0, 1, [1, 2]).unwrap();
    let display_input = client.session(target, None).input().unwrap();
    assert_eq!(display_input.dimensions(), Some([800, 600]));
    assert_eq!(*observed.payloads.lock().unwrap(), b"move 0 1 1 2\n");
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![X11_WINDOW_VERSION, "windows", ":7"],
        vec![X11_WINDOW_VERSION, "capture-window", ":7", "x11-abc"],
        vec![X11_WINDOW_INPUT_VERSION, "input-window", ":7", "x11-abc"],
        vec![DISPLAY_VERSION, "input", ":7"],
      ]
    );
  }

  #[test]
  fn x11_window_input_failure_does_not_open_display_input() {
    let (client, observed) = client(&["", "800 600\n"]);
    let target = Target::X11(7);
    let invalid = client.session(target, Some(window()));
    assert!(matches!(invalid.input(), Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    let session = client.session(
      target,
      Some(Window {
        id: "x11-1".into(),
        ..window()
      }),
    );
    assert!(session.input().is_err());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [[X11_WINDOW_INPUT_VERSION, "input-window", ":7", "x11-1"]]
    );
  }

  #[test]
  fn windows_display_uses_ppm_and_normalized_input_under_its_contract() {
    use crate::media::{Frame, VideoSource};

    let (client, observed) = client(&["P6\n2 1\n255\nabcdef", "ready\n"]);
    let session = client.session(Target::Windows(2), None);
    let mut capture = session.capture().unwrap();
    let mut frame = Frame::default();
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [2, 1]);
    assert_eq!(frame.pixels, b"abcdef");
    let mut input = session.input().unwrap();
    assert_eq!(input.dimensions(), None);
    input.move_pointer(1, 0, [2, 1]).unwrap();
    assert_eq!(*observed.payloads.lock().unwrap(), b"move 1 0 2 1\n");
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![WINDOWS_DISPLAY_VERSION, "capture", "windows-2"],
        vec![WINDOWS_DISPLAY_VERSION, "input", "windows-2"],
      ]
    );
  }

  #[test]
  fn inspection_falls_back_but_an_action_is_sent_once() {
    let (client, observed) = client(&["unsupported contract", "[]", "lost reply"]);
    let session = client.session(Target::Wayland(1), Some(window()));
    assert!(session.elements().unwrap().is_empty());
    assert!(
      session
        .action(Action::SetText {
          path: "/1".into(),
          text: "sample".into()
        })
        .is_err()
    );
    let requests = observed.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0][0], BOUNDS_VERSION);
    assert_eq!(requests[1][0], WAYLAND_WINDOW_VERSION);
    assert_eq!(requests[2][1], "action");
    let payload: serde_json::Value =
      serde_json::from_slice(&observed.payloads.lock().unwrap()).unwrap();
    assert_eq!(payload["text"], "sample");
  }

  #[test]
  fn windows_window_channels_share_selection_and_follow_frame_resizing() {
    use crate::media::{Frame, VideoSource};

    let (client, observed) = client(&["[]", "P6\n2 1\n255\nabcdefP6\n1 2\n255\nghijkl", "ready\n"]);
    assert!(client.windows(Target::Windows(2)).unwrap().is_empty());
    let session = client.session(Target::Windows(2), Some(window()));
    assert!(session.supports_input());
    assert!(!session.supports_semantics());
    let mut capture = session.capture().unwrap();
    let mut frame = Frame::default();
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [2, 1]);
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [1, 2]);
    let mut input = session.input().unwrap();
    input
      .move_pointer(0, 1, frame.size.map(|value| value as u32))
      .unwrap();
    assert_eq!(*observed.payloads.lock().unwrap(), b"move 0 1 1 2\n");
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![WINDOWS_WINDOW_VERSION, "windows", "windows-2"],
        vec![
          WINDOWS_WINDOW_VERSION,
          "capture-window",
          "windows-2",
          "window_1"
        ],
        vec![
          WINDOWS_WINDOW_VERSION,
          "input-window",
          "windows-2",
          "window_1"
        ],
      ]
    );
  }

  #[test]
  fn windows_desktop_window_uses_one_identity_without_a_monitor_for_each_channel() {
    let (client, observed) = client(&[
      r#"[{"id":"window_1","title":"Example","app_id":"example"}]"#,
      r#"[{"id":"windows.wgc","name":"Windows Graphics Capture","scopes":["window"]}]"#,
      "P6\n2 1\n255\nabcdef",
      concat!(
        r#"{"driver":"windows.wgc","format":{"type":"ppm"}}"#,
        "\nP6\n2 1\n255\nabcdef"
      ),
      "ready\n",
    ]);
    let target = Target::WindowsDesktop;
    let mut windows = client.windows(target).unwrap();
    assert_eq!(client.capture_drivers(target).unwrap()[0].id, "windows.wgc");
    let session = client.session(target, windows.pop());
    assert!(session.supports_input());
    assert!(!session.supports_semantics());
    session.capture().unwrap();
    session.capture_with_driver("windows.wgc").unwrap();
    session.input().unwrap();
    let missing = client.session(target, None);
    assert!(!missing.supports_input());
    assert!(missing.capture().is_err());
    assert!(missing.capture_with_driver("auto").is_err());
    assert!(missing.input().is_err());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![WINDOWS_DESKTOP_VERSION, "windows", "windows"],
        vec![WINDOWS_DESKTOP_VERSION, "capture-drivers", "windows"],
        vec![
          WINDOWS_DESKTOP_VERSION,
          "capture-window",
          "windows",
          "window_1"
        ],
        vec![
          WINDOWS_DESKTOP_VERSION,
          "capture-with-driver",
          "windows",
          "windows.wgc",
          "window_1"
        ],
        vec![
          WINDOWS_DESKTOP_VERSION,
          "input-window",
          "windows",
          "window_1"
        ],
      ]
    );
  }

  #[test]
  fn capture_catalog_queries_without_a_target_and_preserves_custom_metadata() {
    use ctream_proto::{CaptureScope, CaptureSystem};
    let (client, observed) = client(&[
      r#"[{"id":"user.surface","name":"User surface","scopes":["display","window"],"systems":["x11","wayland"]}]"#,
    ]);
    let catalog = client.capture_catalog().unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].info.id, "user.surface");
    assert_eq!(
      catalog[0].systems,
      [CaptureSystem::X11, CaptureSystem::Wayland]
    );
    assert_eq!(
      catalog[0].info.scopes,
      [CaptureScope::Display, CaptureScope::Window]
    );
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [vec![MACOS_WINDOW_VERSION, "capture-drivers"]]
    );
  }

  #[test]
  fn macos_discovers_window_identity_and_opens_normalized_input() {
    use crate::media::{Frame, VideoSource};
    use ctream_proto::{CaptureSystem, Display};

    let (client, observed) = client(&[
      r#"[{"display":"macos_desktop","class":"interactive","selectable":true}]"#,
      r#"[{"id":"macos.screencapturekit","name":"ScreenCaptureKit","scopes":["window"],"systems":["macos"]}]"#,
      r#"[{"id":"macos-123-42","title":"Example","app_id":"example"}]"#,
      "{\"driver\":\"macos.screencapturekit\",\"format\":{\"type\":\"ppm\"}}\nP6\n2 1\n255\nabcdefP6\n1 2\n255\nabcdef",
      "ready\n",
    ]);
    assert_eq!(client.discover().unwrap()[0].display, Display::MacosDesktop);
    assert_eq!(
      client.capture_catalog().unwrap()[0].systems,
      [CaptureSystem::Macos]
    );
    let target = Target::MacosDesktop;
    let mut windows = client.windows(target).unwrap();
    let session = client.session(target, windows.pop());
    assert!(session.supports_input());
    assert!(!session.supports_semantics());
    let mut capture = session
      .capture_with_driver("macos.screencapturekit")
      .unwrap();
    let mut frame = Frame::default();
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [2, 1]);
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.size, [1, 2]);
    let mut input = session.input().unwrap();
    input.move_pointer(0, 1, [1, 2]).unwrap();
    assert_eq!(*observed.payloads.lock().unwrap(), b"move 0 1 1 2\n");

    let missing = client.session(target, None);
    assert!(!missing.supports_input());
    assert!(missing.capture().is_err());
    assert!(missing.capture_with_driver("auto").is_err());
    assert!(missing.input().is_err());
    let invalid_id = client.session(target, Some(window()));
    assert!(invalid_id.capture().is_err());
    assert!(invalid_id.capture_with_driver("auto").is_err());
    assert!(invalid_id.input().is_err());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![MACOS_WINDOW_VERSION, "discover"],
        vec![MACOS_WINDOW_VERSION, "capture-drivers"],
        vec![MACOS_WINDOW_VERSION, "windows", "macos"],
        vec![
          MACOS_WINDOW_VERSION,
          "capture-with-driver",
          "macos",
          "macos.screencapturekit",
          "macos-123-42"
        ],
        vec![
          MACOS_WINDOW_VERSION,
          "input-window",
          "macos",
          "macos-123-42"
        ],
      ]
    );
  }

  #[test]
  fn capture_catalog_falls_back_to_its_original_contract() {
    let (client, observed) = client(&["unsupported contract", "[]"]);
    assert!(client.capture_catalog().unwrap().is_empty());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![MACOS_WINDOW_VERSION, "capture-drivers"],
        vec![CAPTURE_CATALOG_VERSION, "capture-drivers"],
      ]
    );
  }

  #[test]
  fn capture_catalog_and_selection_report_actual_driver_without_changing_legacy_capture() {
    use crate::media::{Frame, VideoSource};
    let (client, observed) = client(&[
      r#"[{"id":"user.capture","name":"User provider","scopes":["window"]}]"#,
      "{\"driver\":\"user.capture\",\"format\":{\"type\":\"ppm\"}}\nP6\n1 1\n255\nabc",
      "{\"driver\":\"linux.x11.ffmpeg\",\"format\":{\"type\":\"fixed_rgb\",\"size\":[1,1]}}\nxyz",
    ]);
    assert_eq!(
      client.capture_drivers(Target::Windows(2)).unwrap()[0].id,
      "user.capture"
    );
    let session = client.session(Target::Windows(2), Some(window()));
    let mut capture = session.capture_with_driver("user.capture").unwrap();
    assert_eq!(capture.driver(), Some("user.capture"));
    let mut frame = Frame::default();
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.pixels, b"abc");
    let mut capture = client
      .session(Target::X11(7), None)
      .capture_with_driver("auto")
      .unwrap();
    assert_eq!(capture.driver(), Some("linux.x11.ffmpeg"));
    capture.read_frame(&mut frame).unwrap();
    assert_eq!(frame.pixels, b"xyz");
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![CAPTURE_DRIVER_VERSION, "capture-drivers", "windows-2"],
        vec![
          CAPTURE_DRIVER_VERSION,
          "capture-with-driver",
          "windows-2",
          "user.capture",
          "window_1"
        ],
        vec![CAPTURE_DRIVER_VERSION, "capture-with-driver", ":7", "auto"],
      ]
    );
    assert!(session.capture_with_driver("bad;id").is_err());
    assert_eq!(observed.requests.lock().unwrap().len(), 3);
  }

  #[test]
  fn unsupported_scope_and_invalid_window_never_open_channels() {
    let (client, observed) = client(&[]);
    let session = client.session(Target::Wayland(1), Some(window()));
    assert!(session.input().is_err());
    let session = client.session(Target::X11(1), Some(window()));
    assert!(session.elements().is_err());
    let session = client.session(Target::Windows(1), Some(window()));
    assert!(session.elements().is_err());
    let mut invalid = window();
    invalid.id = "window;command".into();
    assert!(
      client
        .session(Target::Wayland(1), Some(invalid))
        .capture()
        .is_err()
    );
    assert!(observed.requests.lock().unwrap().is_empty());
    let mut invalid = window();
    invalid.id = "window;command".into();
    let session = client.session(Target::Windows(1), Some(invalid));
    assert!(session.capture().is_err());
    assert!(session.input().is_err());
    assert!(observed.requests.lock().unwrap().is_empty());
  }
}
