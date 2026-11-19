use crate::{
  input::{Input, InputFormat},
  media::{Capture, FrameFormat},
  protocol::{
    Action, ActionResult, DISPLAY_VERSION, Element, Session as DisplaySession, Target, VERSION,
    WAYLAND_WINDOW_VERSION, Window, X11_FRAME_SIZE, X11_WINDOW_VERSION, valid_window_id,
  },
  transport::{Channel, Transport},
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
    Ok(vec![DISPLAY_VERSION.into(), "discover".into()])
  }
}

pub struct Windows(pub Target);

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
    self.query(Discover)
  }

  pub fn windows(&self, target: Target) -> io::Result<Vec<Window>> {
    self.query(Windows(target))
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
  pub fn supports_input(&self) -> bool {
    self.window.is_none()
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
        Target::Wayland(_) => FrameFormat::Ppm,
      };
      (
        vec![
          DISPLAY_VERSION.into(),
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
        "pointer and keyboard input require a display session",
      ));
    }
    let format = match self.target {
      Target::X11(_) => InputFormat::X11,
      Target::Wayland(_) => InputFormat::Wayland,
    };
    let arguments = [
      DISPLAY_VERSION.into(),
      "input".into(),
      self.target.to_string(),
    ];
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
        version: VERSION,
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

fn window_version(target: Target) -> &'static str {
  match target {
    Target::X11(_) => X11_WINDOW_VERSION,
    Target::Wayland(_) => WAYLAND_WINDOW_VERSION,
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
        vec!["1.0.1", "discover"],
        vec!["1.0.3", "windows", ":1"],
        vec!["1.0.2", "windows", "wayland-1"],
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
    assert_eq!(requests[0][0], VERSION);
    assert_eq!(requests[1][0], WAYLAND_WINDOW_VERSION);
    assert_eq!(requests[2][1], "action");
    let payload: serde_json::Value =
      serde_json::from_slice(&observed.payloads.lock().unwrap()).unwrap();
    assert_eq!(payload["text"], "sample");
  }

  #[test]
  fn unsupported_scope_and_invalid_window_never_open_channels() {
    let (client, observed) = client(&[]);
    let session = client.session(Target::Wayland(1), Some(window()));
    assert!(session.input().is_err());
    let session = client.session(Target::X11(1), Some(window()));
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
  }
}
