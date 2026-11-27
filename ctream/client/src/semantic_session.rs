//! Attributed, turn-aware accessibility actions on one selected Wayland window.
use crate::{
  Session,
  transport::{Cancellation, Transport},
};
use ctream_proto::{
  Action, ActionResult, TURN_VERSION,
  live::{MAX_TURN_MS, TurnError},
  valid_agent_id,
};
use serde_json::{Value, json};
use std::{
  io::{self, BufRead, BufReader, Read, Write},
  time::{Duration, Instant},
};

const LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticTurn {
  Connecting,
  Legacy,
  Available,
  Held(Instant),
  Expired,
  Disconnected,
}

struct Wire<R, W> {
  reader: BufReader<R>,
  writer: W,
  cancellation: Cancellation,
}

impl<R, W> Drop for Wire<R, W> {
  fn drop(&mut self) {
    self.cancellation.cancel();
  }
}

impl<R: Read, W: Write> Wire<R, W> {
  fn response(&mut self) -> io::Result<Value> {
    let mut bytes = Vec::new();
    self
      .reader
      .by_ref()
      .take((LIMIT + 1) as u64)
      .read_until(b'\n', &mut bytes)?;
    if bytes.pop() != Some(b'\n') {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete or oversized turn-session reply",
      ));
    }
    Ok(serde_json::from_slice(&bytes)?)
  }

  // Outer errors invalidate the stream. A complete host rejection leaves it usable.
  fn exchange(&mut self, body: Value) -> io::Result<io::Result<Value>> {
    let mut bytes = serde_json::to_vec(&body)?;
    if bytes.len() > LIMIT {
      return Ok(Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "semantic request exceeds 64 KiB",
      )));
    }
    bytes.push(b'\n');
    if let Err(error) = self
      .writer
      .write_all(&bytes)
      .and_then(|()| self.writer.flush())
    {
      // Old agents can exit before the capabilities probe reaches their stdin.
      // The process reader supplies the exit diagnostic after stdout reaches EOF.
      if error.kind() == io::ErrorKind::BrokenPipe
        && let Err(diagnostic) = self.response()
      {
        return Err(diagnostic);
      }
      return Err(error);
    }
    let mut reply = self.response()?;
    if reply["ok"] == true
      && let Some(result) = reply.get_mut("result")
    {
      return Ok(Ok(result.take()));
    }
    if reply["ok"] == false
      && let Some(message) = reply["error"].as_str()
    {
      return Ok(Err(match reply["code"].as_str() {
        Some(code @ ("busy" | "stale_turn")) => io::Error::new(
          io::ErrorKind::WouldBlock,
          TurnError {
            code: code.to_owned(),
            owner_id: reply["owner_id"].as_str().map(str::to_owned),
          },
        ),
        _ => io::Error::other(message.to_owned()),
      }));
    }
    Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid turn-session reply",
    ))
  }
}

enum Peer<R, W> {
  Unopened,
  Legacy,
  Current(Wire<R, W>),
  Disconnected,
}
struct Lease {
  token: String,
  deadline: Instant,
}

pub struct SemanticSession<T: Transport> {
  source: Session<T>,
  peer: Peer<T::Reader, T::Writer>,
  lease: Option<Lease>,
}

fn stale() -> io::Error {
  io::Error::new(
    io::ErrorKind::WouldBlock,
    TurnError {
      code: "stale_turn".into(),
      owner_id: None,
    },
  )
}
fn is_stale(error: &io::Error) -> bool {
  error
    .get_ref()
    .and_then(|cause| cause.downcast_ref::<TurnError>())
    .is_some_and(|error| error.code == "stale_turn")
}
fn unsupported(error: &io::Error) -> bool {
  error.to_string().lines().last().is_some_and(|line| {
    line.strip_prefix("ctream-agent: ").unwrap_or(line)
      == "usage: ctream-agent VERSION OPERATION [DISPLAY [WINDOW]]"
  })
}

impl<T: Transport> SemanticSession<T> {
  pub fn new(source: Session<T>) -> Self {
    Self {
      source,
      peer: Peer::Unopened,
      lease: None,
    }
  }

  /// Probe with a read before exposing either input path. Fallback requires
  /// the old agent's command-parser diagnostic. A mutation is never retried.
  pub fn connect(&mut self, label: &str) -> io::Result<()> {
    self.connect_viewer(label, None)
  }

  pub fn connect_viewer(
    &mut self,
    label: &str,
    attachment: Option<&crate::live::ViewerAttachment>,
  ) -> io::Result<()> {
    if !matches!(self.peer, Peer::Unopened) {
      return Err(io::Error::other("semantic control already negotiated"));
    }
    self.peer = Peer::Disconnected;
    let window = self.source.semantic_window()?;
    self.source.validate_target()?;
    if !valid_agent_id(label) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid viewer agent ID",
      ));
    }
    let args = if attachment.is_some() {
      vec![
        ctream_proto::DAEMON_VERSION.into(),
        "viewer-session".into(),
        self.source.target.to_string(),
        window.into(),
      ]
    } else {
      vec![
        TURN_VERSION.into(),
        "turn-session".into(),
        self.source.target.to_string(),
        label.into(),
        window.into(),
      ]
    };
    let probe = self
      .source
      .client
      .transport
      .open(&args)
      .and_then(|mut channel| {
        if let Some(proof) = attachment {
          proof.write_to(&mut channel.writer)?;
        }
        let mut wire = Wire {
          reader: BufReader::new(channel.reader),
          writer: channel.writer,
          cancellation: channel.cancellation,
        };
        let reply = wire.exchange(json!({"operation":"capabilities"}))?;
        Ok((wire, reply))
      });
    match probe {
      Ok((wire, reply)) => {
        if reply?["semantics"] != true {
          return Err(io::Error::other(
            "host does not support semantic control for this window",
          ));
        }
        self.peer = Peer::Current(wire);
      }
      Err(error) if attachment.is_none() && unsupported(&error) => self.peer = Peer::Legacy,
      Err(error) => return Err(error),
    }
    Ok(())
  }

  pub fn state(&self) -> SemanticTurn {
    match self.peer {
      Peer::Unopened => SemanticTurn::Connecting,
      Peer::Legacy => SemanticTurn::Legacy,
      Peer::Disconnected => SemanticTurn::Disconnected,
      Peer::Current(_) => match &self.lease {
        Some(lease) if Instant::now() < lease.deadline => SemanticTurn::Held(lease.deadline),
        Some(_) => SemanticTurn::Expired,
        None => SemanticTurn::Available,
      },
    }
  }

  fn request(&mut self, body: Value) -> io::Result<Value> {
    let Peer::Current(wire) = &mut self.peer else {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "control turns require a connected ctream-agent 1.1.18",
      ));
    };
    match wire.exchange(body) {
      Ok(reply) => reply,
      Err(error) => {
        self.peer = Peer::Disconnected;
        Err(error)
      }
    }
  }

  pub fn acquire(&mut self, ttl_ms: u64) -> io::Result<()> {
    if !(1..=MAX_TURN_MS).contains(&ttl_ms) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid turn duration",
      ));
    }
    let deadline = Instant::now() + Duration::from_millis(ttl_ms);
    let result = self.request(json!({"operation":"acquire_turn","ttl_ms":ttl_ms}))?;
    let Some(token) = result["turn_id"].as_str().filter(|id| !id.is_empty()) else {
      self.peer = Peer::Disconnected;
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "missing semantic turn ID",
      ));
    };
    self.lease = Some(Lease {
      token: token.to_owned(),
      deadline,
    });
    Ok(())
  }

  pub fn release(&mut self) -> io::Result<()> {
    let Some(lease) = &self.lease else {
      return Ok(());
    };
    let result = self.request(json!({"operation":"release_turn","turn_id":lease.token}));
    match result {
      Ok(Value::Null) => {}
      Err(error) if is_stale(&error) => {} // Explicit release clears an expired fence.
      Err(error) => return Err(error),
      Ok(_) => {
        self.peer = Peer::Disconnected;
        return Err(io::Error::other("invalid semantic turn release reply"));
      }
    }
    self.lease = None;
    Ok(())
  }

  pub fn apply(&mut self, action: Action) -> io::Result<ActionResult> {
    if matches!(self.peer, Peer::Legacy) {
      return self.source.action(action);
    }
    let mut body = serde_json::to_value(action)?;
    if let Some(lease) = &self.lease {
      if Instant::now() >= lease.deadline {
        return Err(stale());
      }
      body["turn_id"] = Value::String(lease.token.clone());
    }
    match self.request(body) {
      Ok(Value::Null) => Ok(ActionResult {
        applied: true,
        error: None,
      }),
      Err(error) => {
        if is_stale(&error)
          && let Some(lease) = &mut self.lease
        {
          lease.deadline = Instant::now();
        }
        Err(error)
      }
      Ok(_) => {
        self.peer = Peer::Disconnected;
        Err(io::Error::other("invalid semantic action reply"))
      }
    }
  }
}

#[cfg(test)]
mod tests;
