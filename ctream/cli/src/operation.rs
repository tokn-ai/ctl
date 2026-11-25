use base64::{Engine as _, engine::general_purpose::STANDARD};
use ctream_client::{
  Client, Session,
  automation::{Controls, MouseButton, Point, ScrollDirection},
  media::Frame,
  transport::Transport,
};
use ctream_proto::{Action, Element, Target, Window};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

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

fn screenshot(frame: Frame) -> io::Result<Response> {
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
  drop(frame);
  Ok(Response::Screenshot {
    width,
    height,
    mime_type: "image/png",
    data: STANDARD.encode(png),
  })
}

fn with_controls<T: Transport>(
  session: &Session<T>,
  action: impl FnOnce(&mut Controls<T::Reader, T::Writer>) -> io::Result<()>,
) -> io::Result<Response> {
  let mut controls = session.controls()?;
  action(&mut controls)?;
  controls.finish()?;
  Ok(Response::Done)
}

fn semantic_action<T: Transport>(session: &Session<T>, action: Action) -> io::Result<Response> {
  let result = session.action(action)?;
  if !result.applied {
    return Err(io::Error::other(
      result
        .error
        .unwrap_or_else(|| "agent did not apply the action".into()),
    ));
  }
  Ok(Response::Done)
}

pub(crate) fn run<T: Transport>(
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
  let window = window_id.map(|id| Window {
    id,
    ..Default::default()
  });
  let session = client.session(target, window);
  session.validate_target()?;

  let response = match request {
    Request::Capabilities {} => Response::Capabilities {
      capture: true,
      input: session.supports_input(),
      semantics: session.supports_semantics(),
    },
    Request::Screenshot {} => screenshot(session.screenshot()?)?,
    Request::Elements {} => Response::Elements(session.elements()?),
    Request::Move { point, size } => with_controls(&session, |c| c.move_to(point, size))?,
    Request::Click {
      point,
      size,
      button,
    } => with_controls(&session, |c| c.click(point, size, button))?,
    Request::Drag {
      from,
      to,
      size,
      button,
    } => with_controls(&session, |c| c.drag(from, to, size, button))?,
    Request::Scroll {
      point,
      size,
      direction,
      steps,
    } => with_controls(&session, |c| c.scroll(point, size, direction, steps))?,
    Request::PressKeys { keys } => with_controls(&session, |c| c.press_keys(&keys))?,
    Request::TypeText { text } => with_controls(&session, |c| c.type_text(&text))?,
    Request::Invoke { path, index, name } => {
      semantic_action(&session, Action::Invoke { path, index, name })?
    }
    Request::SetText { path, text } => semantic_action(&session, Action::SetText { path, text })?,
  };
  serde_json::to_writer(&mut output, &response)?;
  output.write_all(b"\n")
}

#[cfg(test)]
mod tests;
