//! An independently cancellable participant channel for the exact selected stream.
use crate::{
  Session,
  transport::{Cancellation, Channel, Transport},
};
use ctream_proto::{
  LIVE_VERSION,
  live::{LiveAction, LiveEvent, RECORD_LIMIT},
  valid_agent_id,
};
use std::io::{self, BufRead, BufReader, Read, Write};

pub fn generate_agent_id() -> String {
  let words: [&str; 32] = serde_json::from_str(include_str!("../../sdk/src/agent-words.json"))
    .expect("invalid bundled agent word list");
  let [index, high, low, ..] = *uuid::Uuid::new_v4().as_bytes();
  let word = words[usize::from(index) % words.len()];
  format!("{word}#{high:02x}{low:02x}")
}

pub struct LiveChannel<R, W> {
  reader: LiveReader<R>,
  writer: LiveWriter<W>,
}

pub struct LiveReader<R> {
  input: BufReader<R>,
  buffer: Vec<u8>,
  joined: bool,
}

pub struct LiveWriter<W> {
  output: W,
  cancel: Cancellation,
}

/// Private capability issued to a viewer's observation channel. Send it only to
/// the control lane for that same host and selection.
#[derive(Clone)]
pub struct ViewerAttachment(String);

impl ViewerAttachment {
  pub fn read(input: &mut impl BufRead) -> io::Result<Self> {
    let mut line = String::new();
    input.take(38).read_line(&mut line)?;
    let id = line
      .strip_suffix('\n')
      .ok_or(io::ErrorKind::UnexpectedEof)?;
    if !uuid::Uuid::try_parse(id).is_ok_and(|value| value.to_string() == id) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid viewer attachment",
      ));
    }
    Ok(Self(id.to_owned()))
  }

  pub fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
    writeln!(writer, "{}", self.0)?;
    writer.flush()
  }

  pub fn as_secret(&self) -> &str {
    &self.0
  }
}

impl<R, W> LiveChannel<R, W> {
  pub fn cancellation(&self) -> Cancellation {
    self.writer.cancel.clone()
  }

  pub fn split(self) -> (LiveReader<R>, LiveWriter<W>) {
    (self.reader, self.writer)
  }
}

impl<R: Read> LiveReader<R> {
  pub fn viewer_attachment(&mut self) -> io::Result<ViewerAttachment> {
    ViewerAttachment::read(&mut self.input)
  }
  pub fn next_event(&mut self) -> io::Result<Option<LiveEvent>> {
    self.buffer.clear();
    if self
      .input
      .by_ref()
      .take((RECORD_LIMIT + 1) as u64)
      .read_until(b'\n', &mut self.buffer)?
      == 0
    {
      return if self.joined {
        Ok(None)
      } else {
        Err(io::Error::new(
          io::ErrorKind::UnexpectedEof,
          "live stream ended before its roster",
        ))
      };
    }
    if self.buffer.pop() != Some(b'\n') {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete or oversized live event",
      ));
    }
    let event: LiveEvent = serde_json::from_slice(&self.buffer)?;
    if !event.valid() || self.joined == matches!(event, LiveEvent::Snapshot { .. }) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid live event or roster",
      ));
    }
    self.joined = true;
    Ok(Some(event))
  }
}

impl<W: Write> LiveWriter<W> {
  pub fn cancellation(&self) -> Cancellation {
    self.cancel.clone()
  }

  pub fn publish(&mut self, action: &LiveAction) -> io::Result<()> {
    if !action.valid() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid live action position",
      ));
    }
    let mut line = serde_json::to_vec(action)?;
    line.push(b'\n');
    self.output.write_all(&line)?;
    self.output.flush()
  }
}

impl<W> Drop for LiveWriter<W> {
  fn drop(&mut self) {
    self.cancel.cancel();
  }
}

impl<T: Transport> Session<T> {
  pub fn live(&self, agent_id: &str) -> io::Result<LiveChannel<T::Reader, T::Writer>> {
    self.open_live(agent_id, LIVE_VERSION, "live-room")
  }

  pub fn live_with_turns(&self, agent_id: &str) -> io::Result<LiveChannel<T::Reader, T::Writer>> {
    self.open_live(agent_id, ctream_proto::TURN_VERSION, "turn-room")
  }

  /// Subscribe to the host daemon's confirmed input events.
  pub fn live_with_daemon(&self, agent_id: &str) -> io::Result<LiveChannel<T::Reader, T::Writer>> {
    self.open_live(agent_id, ctream_proto::DAEMON_VERSION, "turn-room")
  }

  pub fn viewer_observer(&self, label: &str) -> io::Result<LiveChannel<T::Reader, T::Writer>> {
    self.open_live(label, ctream_proto::DAEMON_VERSION, "viewer-observe")
  }

  fn open_live(
    &self,
    agent_id: &str,
    version: &str,
    command: &str,
  ) -> io::Result<LiveChannel<T::Reader, T::Writer>> {
    self.validate_target()?;
    if !valid_agent_id(agent_id) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid agent ID",
      ));
    }
    let mut arguments = vec![
      version.into(),
      command.into(),
      self.target.to_string(),
      agent_id.into(),
    ];
    if let Some(window) = &self.window {
      arguments.push(window.id.clone());
    }
    let Channel {
      reader,
      writer,
      cancellation,
    } = self.client.transport.open(&arguments)?;
    Ok(LiveChannel {
      reader: LiveReader {
        input: BufReader::new(reader),
        buffer: Vec::new(),
        joined: false,
      },
      writer: LiveWriter {
        output: writer,
        cancel: cancellation,
      },
    })
  }
}
