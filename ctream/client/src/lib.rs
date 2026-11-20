//! Typed session operations, transports, and media handling for ctream clients.

pub mod geometry;
pub mod input;
pub mod media;
pub mod options;
pub mod transport;

use crate::{
  input::{Input, InputFormat},
  media::{Capture, FrameFormat},
  transport::{Channel, Transport},
};
use ctream_proto::{
  Action, ActionResult, BOUNDS_VERSION, CAPTURE_DRIVER_VERSION, CaptureDriverInfo, DISPLAY_VERSION,
  Element, Session as DisplaySession, Target, WAYLAND_WINDOW_VERSION, WINDOWS_DISPLAY_VERSION,
  WINDOWS_WINDOW_VERSION, Window, X11_FRAME_SIZE, X11_WINDOW_VERSION, valid_capture_driver,
  valid_window_id,
};
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
    Ok(vec![WINDOWS_DISPLAY_VERSION.into(), "discover".into()])
  }
}

struct LegacyDiscover;

impl Query for LegacyDiscover {
  type Reply = Vec<DisplaySession>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![DISPLAY_VERSION.into(), "discover".into()])
  }
}

pub struct Windows(pub Target);

pub struct CaptureDrivers(pub Target);

impl Query for CaptureDrivers {
  type Reply = Vec<CaptureDriverInfo>;

  fn arguments(&self) -> io::Result<Vec<String>> {
    Ok(vec![
      CAPTURE_DRIVER_VERSION.into(),
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
      .query(Discover)
      .or_else(|error| self.query(LegacyDiscover).map_err(|_| error))
  }

  pub fn windows(&self, target: Target) -> io::Result<Vec<Window>> {
    self.query(Windows(target))
  }

  pub fn capture_drivers(&self, target: Target) -> io::Result<Vec<CaptureDriverInfo>> {
    self.query(CaptureDrivers(target))
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
  pub fn capture_with_driver(&self, driver: &str) -> io::Result<Capture<T::Reader, T::Writer>> {
    if !valid_capture_driver(driver) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid capture driver identifier",
      ));
    }
    let mut arguments = vec![
      CAPTURE_DRIVER_VERSION.into(),
      "capture-with-driver".into(),
      self.target.to_string(),
      driver.into(),
    ];
    if let Some(window) = &self.window {
      if !valid_window_id(&window.id) {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "invalid window identifier",
        ));
      }
      arguments.push(window.id.clone());
    }
    Capture::started(self.client.transport.open(&arguments)?, driver)
  }

  pub fn supports_input(&self) -> bool {
    self.window.is_none() || matches!(self.target, Target::Windows(_))
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
    if !self.supports_input() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "pointer and keyboard input require a display or Windows window session",
      ));
    }
    let format = match self.target {
      Target::X11(_) => InputFormat::X11,
      Target::Wayland(_) => InputFormat::Wayland,
      Target::Windows(_) => InputFormat::Windows,
    };
    let arguments = if let Some(window) = &self.window {
      window_arguments(
        WINDOWS_WINDOW_VERSION,
        "input-window",
        self.target,
        &window.id,
      )?
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

fn display_version(target: Target) -> &'static str {
  if matches!(target, Target::Windows(_)) {
    WINDOWS_DISPLAY_VERSION
  } else {
    DISPLAY_VERSION
  }
}

fn window_version(target: Target) -> &'static str {
  match target {
    Target::X11(_) => X11_WINDOW_VERSION,
    Target::Wayland(_) => WAYLAND_WINDOW_VERSION,
    Target::Windows(_) => WINDOWS_WINDOW_VERSION,
  }
}

fn window_arguments(
  version: &str,
  operation: &str,
  target: Target,
  id: &str,
) -> io::Result<Vec<String>> {
  if !valid_window_id(id) {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "invalid window identifier",
    ));
  }
  Ok(vec![
    version.into(),
    operation.into(),
    target.to_string(),
    id.into(),
  ])
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
    let (client, observed) = client(&["unsupported contract", "[]"]);
    assert!(client.discover().unwrap().is_empty());
    assert_eq!(
      *observed.requests.lock().unwrap(),
      [
        vec![WINDOWS_DISPLAY_VERSION, "discover"],
        vec![DISPLAY_VERSION, "discover"],
      ]
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
