//! Serve one-shot and persistent JSON operations for a selected graphical source.

use crate::{
  Client, Session,
  automation::{Automation, Controls, MouseButton, Point, ScrollDirection},
  media::Frame,
  transport::Transport,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ctream_proto::{Action, Element, Target, Window, live::LiveAction};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};

const REQUEST_LIMIT: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(untagged)]
enum Request {
  Observe(Observation),
  Input(Mutation),
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Observation {
  Capabilities {},
  Screenshot {},
  Elements {},
}

/// A complete input request for a selected display or window.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mutation {
  Move {
    point: Point,
    size: [u32; 2],
  },
  Click {
    point: Point,
    size: [u32; 2],
    button: MouseButton,
  },
  Drag {
    from: Point,
    to: Point,
    size: [u32; 2],
    button: MouseButton,
  },
  Scroll {
    point: Point,
    size: [u32; 2],
    direction: ScrollDirection,
    steps: u16,
  },
  PressKeys {
    keys: Vec<String>,
  },
  TypeText {
    text: String,
  },
  Invoke {
    path: String,
    index: usize,
    name: String,
  },
  SetText {
    path: String,
    text: String,
  },
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
  // Consumed by the persistent reply envelope, never serialized as its result.
  Applied(String),
  Turn {
    turn_id: String,
  },
  Capabilities {
    capture: bool,
    input: bool,
    semantics: bool,
  },
  Screenshot {
    width: u32,
    height: u32,
    mime_type: &'static str,
    data: String,
  },
  Elements(Vec<Element>),
  Done,
}

fn screenshot(frame: &Frame) -> io::Result<Response> {
  let [width, height] = frame.size;
  let dimension = |size| {
    u32::try_from(size)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame exceeds PNG dimensions"))
  };
  let (width, height) = (dimension(width)?, dimension(height)?);
  let mut png = Vec::new();
  let mut encoder = png::Encoder::new(&mut png, width, height);
  encoder.set_color(png::ColorType::Rgb);
  encoder.set_depth(png::BitDepth::Eight);
  encoder.set_compression(png::Compression::Fast);
  let mut writer = encoder.write_header()?;
  writer.write_image_data(&frame.pixels)?;
  writer.finish()?;
  Ok(Response::Screenshot {
    width,
    height,
    mime_type: "image/png",
    data: STANDARD.encode(png),
  })
}

enum Execution<'a, T: Transport> {
  Once(&'a Session<T>),
  Live(&'a mut Automation<T>),
}

/// The target application answered an accessibility request and rejected it.
#[derive(Debug)]
pub struct SemanticRefusal(pub String);
impl std::fmt::Display for SemanticRefusal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}
impl std::error::Error for SemanticRefusal {}

impl<T: Transport> Execution<'_, T> {
  fn selection(&self) -> &Session<T> {
    match self {
      Self::Once(session) => session,
      Self::Live(automation) => automation.selection(),
    }
  }

  fn observe(&mut self) -> io::Result<Response> {
    match self {
      Self::Once(session) => screenshot(&session.screenshot()?),
      Self::Live(automation) => screenshot(automation.screenshot()?.as_ref()),
    }
  }

  fn with_controls(
    &mut self,
    action: impl FnOnce(&mut Controls<T::Reader, T::Writer>) -> io::Result<()>,
  ) -> io::Result<()> {
    match self {
      Self::Once(session) => {
        let mut controls = session.controls()?;
        action(&mut controls)?;
        controls.finish()?;
      }
      Self::Live(automation) => automation.with_controls(action)?,
    }
    Ok(())
  }

  fn semantic_action(&mut self, action: Action) -> io::Result<()> {
    let result = match self {
      Self::Once(session) => session.action(action)?,
      Self::Live(automation) => automation.action(action)?,
    };
    if !result.applied {
      return Err(io::Error::other(SemanticRefusal(
        result
          .error
          .unwrap_or_else(|| "agent did not apply the action".into()),
      )));
    }
    Ok(())
  }
}

/// Serve a single ctream_sdk 1.0.1 request, returning its unwrapped result.
pub fn run<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  mut output: impl Write,
) -> io::Result<()> {
  let mut bytes = Vec::new();
  input
    .take((REQUEST_LIMIT + 1) as u64)
    .read_to_end(&mut bytes)?;
  if bytes.len() > REQUEST_LIMIT {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "operation exceeds 64 KiB",
    ));
  }
  let request: Request = serde_json::from_slice(&bytes)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
  let session = select(client, target, window_id);
  session.validate_target()?;
  let response = execute(&mut Execution::Once(&session), request, &mut |_, apply| {
    apply()
  })?;
  serde_json::to_writer(&mut output, &response)?;
  output.write_all(b"\n")
}

fn select<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
) -> Session<T> {
  let window = window_id.map(|id| Window {
    id,
    ..Default::default()
  });
  client.session(target, window)
}

type Recorder<'a> =
  dyn FnMut(LiveAction, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> + 'a;

fn mutation(
  record: &mut Recorder<'_>,
  operation: LiveAction,
  apply: impl FnOnce() -> io::Result<()>,
) -> io::Result<Response> {
  let mut pending = Some(apply);
  let mut dispatch_error = None;
  let recorded = record(operation, &mut || {
    let apply = pending
      .take()
      .ok_or_else(|| io::Error::other("operation already dispatched"))?;
    apply().map_err(|error| {
      let report = io::Error::new(error.kind(), error.to_string());
      let report = if ctream_driver::UnconfirmedDelivery::is(&error) {
        ctream_driver::UnconfirmedDelivery::error(report)
      } else {
        report
      };
      dispatch_error = Some(error);
      report
    })
  });
  if let Some(error) = dispatch_error {
    return Err(error);
  }
  recorded?;
  if pending.is_some() {
    return Err(io::Error::other("recorder did not dispatch the operation"));
  }
  Ok(Response::Done)
}

// Pixel centers map to the same point at any viewer scale.
fn normalized(point: Point, [width, height]: [u32; 2]) -> [f32; 2] {
  [
    (point.x as f32 + 0.5) / width as f32,
    (point.y as f32 + 0.5) / height as f32,
  ]
}

impl Mutation {
  /// Public overlay metadata. Text, key names, and accessibility arguments stay private.
  pub fn overlay(&self) -> LiveAction {
    match self {
      Self::Move { point, size } => LiveAction::Move {
        point: normalized(*point, *size),
      },
      Self::Click { point, size, .. } => LiveAction::Click {
        point: normalized(*point, *size),
      },
      Self::Drag { from, to, size, .. } => LiveAction::Drag {
        from: normalized(*from, *size),
        to: normalized(*to, *size),
      },
      Self::Scroll { point, size, .. } => LiveAction::Scroll {
        point: normalized(*point, *size),
      },
      Self::PressKeys { .. } => LiveAction::PressKeys {},
      Self::TypeText { .. } => LiveAction::TypeText {},
      Self::Invoke { .. } => LiveAction::Invoke {},
      Self::SetText { .. } => LiveAction::SetText {},
    }
  }

  /// Execute on the selected desktop worker, reusing its input channel.
  pub fn apply<T: Transport>(self, automation: &mut Automation<T>) -> io::Result<()> {
    self.apply_to(&mut Execution::Live(automation))
  }

  fn apply_to<T: Transport>(self, execution: &mut Execution<'_, T>) -> io::Result<()> {
    match self {
      Self::Move { point, size } => execution.with_controls(|c| c.move_to(point, size)),
      Self::Click {
        point,
        size,
        button,
      } => execution.with_controls(|c| c.click(point, size, button)),
      Self::Drag {
        from,
        to,
        size,
        button,
      } => execution.with_controls(|c| c.drag(from, to, size, button)),
      Self::Scroll {
        point,
        size,
        direction,
        steps,
      } => execution.with_controls(|c| c.scroll(point, size, direction, steps)),
      Self::PressKeys { keys } => execution.with_controls(|c| c.press_keys(&keys)),
      Self::TypeText { text } => execution.with_controls(|c| c.type_text(&text)),
      Self::Invoke { path, index, name } => {
        execution.semantic_action(Action::Invoke { path, index, name })
      }
      Self::SetText { path, text } => execution.semantic_action(Action::SetText { path, text }),
    }
  }
}

fn execute<T: Transport>(
  execution: &mut Execution<'_, T>,
  request: Request,
  record: &mut Recorder<'_>,
) -> io::Result<Response> {
  match request {
    Request::Observe(read) => observe(execution, read),
    Request::Input(action) => mutation(record, action.overlay(), || action.apply_to(execution)),
  }
}
fn observe<T: Transport>(
  execution: &mut Execution<'_, T>,
  read: Observation,
) -> io::Result<Response> {
  match read {
    Observation::Capabilities {} => Ok(Response::Capabilities {
      capture: true,
      input: execution.selection().supports_input(),
      semantics: execution.selection().supports_semantics(),
    }),
    Observation::Screenshot {} => execution.observe(),
    Observation::Elements {} => execution.selection().elements().map(Response::Elements),
  }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Reply {
  Success {
    ok: bool,
    result: Response,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_id: Option<String>,
  },
  Failure {
    ok: bool,
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action_id: Option<String>,
  },
}

/// Connection-bound authority for the opt-in turn protocol. Reads never enter admission.
pub trait SessionAuthority {
  fn acquire(&mut self, ttl_ms: u64) -> io::Result<String>;
  fn release(&mut self, turn_id: &str) -> io::Result<()>;
  fn dispatch(&mut self, turn_id: Option<&str>, action: Mutation) -> io::Result<String>;
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum TurnCommand {
  AcquireTurn {
    #[serde(default = "default_ttl")]
    ttl_ms: u64,
  },
  ReleaseTurn {
    turn_id: String,
  },
}

fn default_ttl() -> u64 {
  ctream_proto::live::DEFAULT_TURN_MS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionProtocol {
  Attributed,
  Turns,
  Daemon,
}

pub fn run_host_session<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  output: impl Write,
  protocol: SessionProtocol,
  authority: &mut impl SessionAuthority,
) -> io::Result<()> {
  session_loop(
    client,
    target,
    window_id,
    input,
    output,
    protocol,
    |automation, line| {
      let mut value: serde_json::Value = serde_json::from_slice(line)?;
      if protocol != SessionProtocol::Attributed
        && matches!(
          value.get("operation").and_then(|op| op.as_str()),
          Some("acquire_turn" | "release_turn")
        )
      {
        return match serde_json::from_value(value)? {
          TurnCommand::AcquireTurn { ttl_ms } => authority
            .acquire(ttl_ms)
            .map(|turn_id| Response::Turn { turn_id }),
          TurnCommand::ReleaseTurn { turn_id } => {
            authority.release(&turn_id).map(|()| Response::Done)
          }
        };
      }
      let fields = value.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected a session operation")
      })?;
      let turn_id: Option<String> = if protocol != SessionProtocol::Attributed {
        fields
          .remove("turn_id")
          .map(serde_json::from_value)
          .transpose()?
      } else {
        None
      };
      match serde_json::from_value(value)? {
        Request::Observe(_) if turn_id.is_some() => Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "turn_id is only valid on input operations",
        )),
        Request::Observe(read) => observe(&mut Execution::Live(automation), read),
        Request::Input(action) => {
          automation.prepare_remote_action()?;
          authority
            .dispatch(turn_id.as_deref(), action)
            .map(Response::Applied)
        }
      }
    },
  )
}

/// Serve one selected source until the caller closes stdin. Each reply is
/// flushed before reading another request, including recoverable operation errors.
pub fn run_session<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  output: impl Write,
) -> io::Result<()> {
  run_attributed_session(client, target, window_id, input, output, |_, apply| apply())
}

/// Serve a persistent session whose recorder gates each physical or semantic
/// mutation. The recorder invokes `apply` once to dispatch the named operation
/// and can return an error before dispatch. Reads bypass the recorder.
pub fn run_attributed_session<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  output: impl Write,
  mut record: impl FnMut(&'static str, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
) -> io::Result<()> {
  run_participant_session(client, target, window_id, input, output, |action, apply| {
    record(action.operation(), apply)
  })
}

pub fn run_participant_session<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  output: impl Write,
  mut record: impl FnMut(LiveAction, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
) -> io::Result<()> {
  session_loop(
    client,
    target,
    window_id,
    input,
    output,
    SessionProtocol::Attributed,
    |automation, line| {
      execute(
        &mut Execution::Live(automation),
        serde_json::from_slice(line)?,
        &mut record,
      )
    },
  )
}

fn session_loop<T: Transport>(
  client: &Client<T>,
  target: Target,
  window_id: Option<String>,
  input: impl Read,
  mut output: impl Write,
  protocol: SessionProtocol,
  mut process: impl FnMut(&mut Automation<T>, &[u8]) -> io::Result<Response>,
) -> io::Result<()> {
  let mut automation = select(client, target, window_id).automation()?;
  let mut reader = BufReader::new(input);
  let mut line = Vec::new();
  loop {
    line.clear();
    if reader
      .by_ref()
      .take((REQUEST_LIMIT + 1) as u64)
      .read_until(b'\n', &mut line)?
      == 0
    {
      break;
    }
    if line.pop() != Some(b'\n') {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "expected a newline after at most 64 KiB",
      ));
    }
    let result = process(&mut automation, &line);
    let reply = match result {
      Ok(Response::Applied(id)) => Reply::Success {
        ok: true,
        result: Response::Done,
        action_id: (protocol == SessionProtocol::Daemon).then_some(id),
      },
      Ok(result) => Reply::Success {
        ok: true,
        result,
        action_id: None,
      },
      Err(error) => {
        let failure = error
          .get_ref()
          .and_then(|cause| cause.downcast_ref::<ctream_proto::live::ActionFailure>());
        let turn = failure
          .map(|failure| &failure.source)
          .unwrap_or(&error)
          .get_ref()
          .and_then(|e| e.downcast_ref::<ctream_proto::live::TurnError>())
          .filter(|_| protocol != SessionProtocol::Attributed);
        Reply::Failure {
          ok: false,
          error: error.to_string(),
          code: turn.map(|e| e.code.clone()),
          owner_id: turn.and_then(|e| e.owner_id.clone()),
          action_id: failure
            .filter(|_| protocol == SessionProtocol::Daemon)
            .map(|failure| failure.action_id.clone()),
        }
      }
    };
    serde_json::to_writer(&mut output, &reply)?;
    output.write_all(b"\n")?;
    output.flush()?;
  }
  automation.finish()
}

#[cfg(test)]
mod tests;
