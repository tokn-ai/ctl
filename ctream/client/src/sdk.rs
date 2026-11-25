//! Serve one-shot and persistent JSON operations for a selected graphical source.

use crate::{
  Client, Session,
  automation::{Automation, Controls, MouseButton, Point, ScrollDirection},
  media::Frame,
  transport::Transport,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ctream_proto::{Action, Element, Target, Window};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, BufReader, Read, Write};

const REQUEST_LIMIT: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
  Capabilities {},
  Screenshot {},
  Elements {},
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
      return Err(io::Error::other(
        result
          .error
          .unwrap_or_else(|| "agent did not apply the action".into()),
      ));
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
  dyn FnMut(&'static str, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()> + 'a;

fn mutation(
  record: &mut Recorder<'_>,
  operation: &'static str,
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

fn execute<T: Transport>(
  execution: &mut Execution<'_, T>,
  request: Request,
  record: &mut Recorder<'_>,
) -> io::Result<Response> {
  match request {
    Request::Capabilities {} => Ok(Response::Capabilities {
      capture: true,
      input: execution.selection().supports_input(),
      semantics: execution.selection().supports_semantics(),
    }),
    Request::Screenshot {} => execution.observe(),
    Request::Elements {} => execution.selection().elements().map(Response::Elements),
    Request::Move { point, size } => mutation(record, "move", || {
      execution.with_controls(|c| c.move_to(point, size))
    }),
    Request::Click {
      point,
      size,
      button,
    } => mutation(record, "click", || {
      execution.with_controls(|c| c.click(point, size, button))
    }),
    Request::Drag {
      from,
      to,
      size,
      button,
    } => mutation(record, "drag", || {
      execution.with_controls(|c| c.drag(from, to, size, button))
    }),
    Request::Scroll {
      point,
      size,
      direction,
      steps,
    } => mutation(record, "scroll", || {
      execution.with_controls(|c| c.scroll(point, size, direction, steps))
    }),
    Request::PressKeys { keys } => mutation(record, "press_keys", || {
      execution.with_controls(|c| c.press_keys(&keys))
    }),
    Request::TypeText { text } => mutation(record, "type_text", || {
      execution.with_controls(|c| c.type_text(&text))
    }),
    Request::Invoke { path, index, name } => mutation(record, "invoke", || {
      execution.semantic_action(Action::Invoke { path, index, name })
    }),
    Request::SetText { path, text } => mutation(record, "set_text", || {
      execution.semantic_action(Action::SetText { path, text })
    }),
  }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Reply {
  Success { ok: bool, result: Response },
  Failure { ok: bool, error: String },
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
  mut output: impl Write,
  mut record: impl FnMut(&'static str, &mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
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
    let result = serde_json::from_slice(&line)
      .map_err(io::Error::from)
      .and_then(|request| execute(&mut Execution::Live(&mut automation), request, &mut record));
    let reply = match result {
      Ok(result) => Reply::Success { ok: true, result },
      Err(error) => Reply::Failure {
        ok: false,
        error: error.to_string(),
      },
    };
    serde_json::to_writer(&mut output, &reply)?;
    output.write_all(b"\n")?;
    output.flush()?;
  }
  automation.finish()
}

#[cfg(test)]
mod tests;
