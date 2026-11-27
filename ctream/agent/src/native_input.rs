//! Desktop admission for native pointer and keyboard gestures. Keep admission
//! from the first delivered down through the last release, including EOF cleanup.
use crate::{
  daemon::{Authority, Delegate},
  desktop_input::InputRequest,
};
use ctream_proto::Target;
use ctream_proto::live::TurnError;
use std::{
  collections::BTreeSet,
  io::{self, BufRead, Read, Write},
  path::Path,
  sync::mpsc,
  time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
mod helper;
#[cfg(target_os = "macos")]
pub mod picker;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use helper::guard_with;
#[cfg(target_os = "linux")]
pub use helper::{LinuxHelperGuard, monitor_linux_helper};

const LINE_LIMIT: usize = 128;
pub const BATCH_LIMIT: usize = 256;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Mode {
  Legacy,
  Synchronized,
  Viewer { receipts: bool },
}

/// Platform-side fence, kept across held input and cleanup.
pub struct Gate {
  controller: Delegate,
  held: BTreeSet<(bool, String)>,
  active: bool,
  held_until: Option<Instant>,
}

impl Gate {
  pub fn delegated(target: Target, window: Option<&str>, key: &str) -> io::Result<Self> {
    Ok(Self {
      controller: Delegate::connect(target, window, key)?,
      active: false,
      held_until: None,
      held: BTreeSet::new(),
    })
  }
  #[cfg(target_os = "linux")]
  pub fn helper_guard(&mut self) -> io::Result<LinuxHelperGuard> {
    Ok(LinuxHelperGuard(self.controller.helper_handles()?))
  }
  pub fn finish(&mut self) -> io::Result<()> {
    if self.active {
      self.controller.finish()?;
      self.active = false;
    }
    Ok(())
  }
  fn begin(&mut self) -> io::Result<()> {
    if !self.active {
      self.controller.begin()?;
      self.active = true;
    }
    Ok(())
  }
  fn apply(
    &mut self,
    commands: &[String],
    dispatch: &mut impl FnMut(&[String]) -> io::Result<()>,
  ) -> io::Result<()> {
    self.begin()?;
    dispatch(commands)?;
    update_held(&mut self.held, commands);
    if self.held.is_empty() {
      self.held_until = None;
      self.finish()?;
    } else {
      self.held_until.get_or_insert_with(|| {
        Instant::now() + Duration::from_millis(ctream_proto::live::MAX_TURN_MS)
      });
    }
    Ok(())
  }
  pub fn semantic(&mut self, apply: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    self.begin()?;
    let result = apply();
    result.and(self.finish())
  }
}

/// Track the same acknowledged edges in the daemon and in its independently fenced worker.
pub(crate) fn update_held(held: &mut BTreeSet<(bool, String)>, commands: &[String]) {
  for command in commands {
    let mut words = command.split_ascii_whitespace();
    match (words.next(), words.next(), words.next()) {
      (Some("reset"), None, None) => held.clear(),
      (Some(kind @ ("button" | "key")), Some(code), Some(edge)) => {
        let item = (kind == "key", code.to_owned());
        if edge == "1" {
          held.insert(item);
        } else {
          held.remove(&item);
        }
      }
      _ => {}
    }
  }
}

pub(crate) fn operation(lines: &[String]) -> &'static str {
  let mut kind = "move";
  for line in lines {
    let mut words = line.split_ascii_whitespace();
    match (words.next(), words.next(), words.next()) {
      (Some("button"), Some("4" | "5" | "6" | "7"), _) => return "scroll",
      (Some("button"), _, Some("1")) => return "pointer_down",
      (Some("button"), _, Some("0")) => return "pointer_up",
      (Some("key"), _, _) => kind = "press_keys",
      (Some("reset"), _, _) => kind = "reset",
      _ => {}
    }
  }
  kind
}
fn line(input: &mut impl BufRead) -> io::Result<Option<String>> {
  let mut line = String::new();
  if input.take((LINE_LIMIT + 1) as u64).read_line(&mut line)? == 0 {
    return Ok(None);
  }
  if line.len() > LINE_LIMIT || !line.ends_with('\n') {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "incomplete or oversized input line",
    ));
  }
  Ok(Some(line))
}

pub(crate) fn validate_event(command: &str) -> io::Result<()> {
  if command.len() > LINE_LIMIT
    || !command.ends_with('\n')
    || command[..command.len() - 1].contains(['\r', '\n'])
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "invalid native input line",
    ));
  }
  let mut words = command.split_ascii_whitespace();
  let valid = match words.next() {
    Some("reset") => words.next().is_none(),
    Some("move") => {
      let coords: Vec<_> = words.collect();
      matches!(coords.len(), 2 | 4) && coords.iter().all(|s| s.parse::<u32>().is_ok())
    }
    Some("button") => {
      words.next().is_some_and(|s| s.parse::<u8>().is_ok())
        && matches!(words.next(), Some("0" | "1"))
        && words.next().is_none()
    }
    Some("key") => {
      words
        .next()
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        && matches!(words.next(), Some("0" | "1"))
        && words.next().is_none()
    }
    _ => false,
  };
  if valid {
    Ok(())
  } else {
    Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "unknown input event",
    ))
  }
}

enum Packet {
  Sync,
  Batch {
    commands: Vec<String>,
    scoped: bool,
    acknowledge: bool,
  },
  Acquire(u64),
  Release,
}

fn read_packet(
  mode: Mode,
  delegated: bool,
  input: &mut impl BufRead,
) -> io::Result<Option<Packet>> {
  let Some(command) = line(input)? else {
    return Ok(None);
  };
  if command == "sync\n" && mode == Mode::Synchronized {
    return Ok(Some(Packet::Sync));
  }
  if !matches!(mode, Mode::Viewer { .. }) {
    validate_event(&command)?;
    let mut commands = vec![command];
    // SDKs fence complete actions. Avoid requesting admission and a
    // backend round-trip for each individual key in an already-admitted batch.
    let mut acknowledge = false;
    if delegated && mode == Mode::Synchronized {
      while commands.len() < BATCH_LIMIT {
        let Some(next) = line(input)? else { break };
        if next == "sync\n" {
          acknowledge = true;
          break;
        }
        validate_event(&next)?;
        commands.push(next);
      }
    }
    return Ok(Some(Packet::Batch {
      commands,
      scoped: false,
      acknowledge,
    }));
  }
  let words: Vec<_> = command.split_ascii_whitespace().collect();
  let (count, scoped) = match words.as_slice() {
    ["take-turn", ttl] => {
      let ttl = ttl
        .parse::<u64>()
        .ok()
        .filter(|ttl| (1..=ctream_proto::live::MAX_TURN_MS).contains(ttl))
        .ok_or_else(|| io::Error::other("invalid turn duration"))?;
      return Ok(Some(Packet::Acquire(ttl)));
    }
    ["release-turn"] => return Ok(Some(Packet::Release)),
    ["batch", count] => (*count, false),
    ["batch", "turn", count] => (*count, true),
    _ => return Err(io::Error::other("invalid viewer input request")),
  };
  let count = count
    .parse::<usize>()
    .ok()
    .filter(|n| (1..=BATCH_LIMIT).contains(n))
    .ok_or_else(|| io::Error::other("invalid viewer input batch"))?;
  let mut commands = Vec::with_capacity(count);
  for _ in 0..count {
    let command = line(input)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    validate_event(&command)?;
    commands.push(command);
  }
  Ok(Some(Packet::Batch {
    commands,
    scoped,
    acknowledge: true,
  }))
}

/// Translate a retained native channel to complete daemon requests. Drivers
/// and their input state stay on the daemon side of this connection.
pub fn serve_controller(
  mode: Mode,
  mut authority: Authority,
  picker: Option<&str>,
  lease: Option<&str>,
  mut input: impl BufRead,
  output: &mut impl Write,
) -> io::Result<()> {
  let header = authority.prepare_raw(picker, lease)?;
  output.write_all(header.as_bytes())?;
  output.flush()?;
  let mut token: Option<String> = None;
  let result = (|| {
    while let Some(packet) = read_packet(mode, false, &mut input)? {
      let (reply, acknowledge) = match packet {
        Packet::Sync => (Ok(None), true),
        Packet::Acquire(ttl) => (
          authority.acquire(ttl).map(|id| {
            token = Some(id);
            None
          }),
          true,
        ),
        Packet::Release => (
          authority
            .release(token.take().as_deref().unwrap_or(""))
            .map(|()| None),
          true,
        ),
        Packet::Batch {
          commands,
          scoped,
          acknowledge,
        } => {
          let scope = scoped.then(|| token.as_deref().unwrap_or(""));
          let action = authority.execute(scope, InputRequest::Raw { commands });
          (action.map(Some), acknowledge)
        }
      };
      match reply {
        Ok(Some(id)) if mode == (Mode::Viewer { receipts: true }) => writeln!(output, "ok {id}")?,
        Ok(_) if acknowledge => output.write_all(b"ok\n")?,
        Ok(_) => {}
        Err(error) => {
          let failure = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<ctream_proto::live::ActionFailure>());
          let inner = failure.map_or(&error, |f| &f.source);
          let turn = inner.get_ref().and_then(|e| e.downcast_ref::<TurnError>());
          let Mode::Viewer { receipts } = mode else {
            return Err(error);
          };
          let mut wire = if let Some(turn) = turn {
            if turn.code == "stale_turn" {
              token = None;
            }
            serde_json::json!({"error":turn})
          } else if receipts && failure.is_some() {
            let code = if ctream_driver::UnconfirmedDelivery::is(inner) {
              "unknown"
            } else {
              "failed"
            };
            let mut message: String = inner.to_string().chars().take(240).collect();
            message.retain(|c| !c.is_control());
            serde_json::json!({"error":{"code":code,"message":message}})
          } else {
            return Err(error);
          };
          if let Some(failure) = failure.filter(|_| receipts) {
            wire["action_id"] = serde_json::Value::String(failure.action_id.clone());
          }
          serde_json::to_writer(&mut *output, &wire)?;
          output.write_all(b"\n")?;
        }
      }
      if acknowledge {
        output.flush()?;
      }
    }
    Ok(())
  })();
  let closed = authority.close();
  result.and(closed)
}

pub fn controller(
  target: Target,
  window: Option<&str>,
  label: Option<&str>,
  exe: Option<&Path>,
) -> io::Result<Authority> {
  let id = uuid::Uuid::new_v4().to_string();
  let generated;
  let label = match label {
    Some(label) => label,
    None => {
      generated = ctream_client::generate_agent_id();
      &generated
    }
  };
  match exe {
    Some(exe) => Authority::connect_via(exe, target, window, label, &id),
    None => Authority::connect(target, window, label, &id),
  }
}

enum Incoming {
  Input(io::Result<Option<Packet>>, mpsc::Sender<()>),
  #[cfg(any(target_os = "linux", windows))]
  DriverEnded,
}

fn feed(
  mode: Mode,
  delegated: bool,
  mut input: impl BufRead + Send + 'static,
  sender: mpsc::Sender<Incoming>,
) -> io::Result<()> {
  std::thread::Builder::new()
    .name("ctream-native-events".into())
    .spawn(move || {
      loop {
        let next = read_packet(mode, delegated, &mut input);
        let end = !matches!(next, Ok(Some(_)));
        let (done, wait) = mpsc::channel();
        if sender.send(Incoming::Input(next, done)).is_err() || end || wait.recv().is_err() {
          break;
        }
      }
    })?;
  Ok(())
}

fn listen(
  gate: &mut Gate,
  requests: mpsc::Receiver<Incoming>,
  output: &mut impl Write,
  mut dispatch: impl FnMut(&[String]) -> io::Result<()>,
) -> io::Result<()> {
  loop {
    let next = if let Some(deadline) = gate.held_until {
      match requests.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(v) => v,
        Err(mpsc::RecvTimeoutError::Timeout) => {
          dispatch(&["reset\n".into()])?;
          gate.held.clear();
          gate.finish()?;
          return Ok(());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
      }
    } else {
      match requests.recv() {
        Ok(v) => v,
        Err(_) => return Ok(()),
      }
    };
    let (next, done) = match next {
      Incoming::Input(next, done) => (next, done),
      #[cfg(any(target_os = "linux", windows))]
      Incoming::DriverEnded => return Ok(()),
    };
    let Some(packet) = next? else {
      return Ok(());
    };
    let acknowledge = match packet {
      Packet::Sync => {
        dispatch(&[])?;
        true
      }
      Packet::Batch {
        commands,
        acknowledge,
        ..
      } => {
        gate.apply(&commands, &mut dispatch)?;
        acknowledge
      }
      _ => return Err(io::Error::other("a platform worker cannot take a turn")),
    };
    if acknowledge {
      output.write_all(b"ok\n")?;
    }
    if acknowledge {
      output.flush()?;
    }
    let _ = done.send(());
  }
}

pub fn receive(
  mode: Mode,
  gate: &mut Gate,
  input: impl BufRead + Send + 'static,
  output: &mut impl Write,
  dispatch: impl FnMut(&[String]) -> io::Result<()>,
) -> io::Result<()> {
  let (send, requests) = mpsc::channel();
  feed(mode, true, input, send)?;
  listen(gate, requests, output, dispatch)
}

/// Keep native parsing and delivery inside the stock backend. Each admitted
/// event/batch is fenced with the backend's synchronization operation.
#[cfg(any(test, target_os = "linux", windows))]
fn dispatch_native(
  writer: &mut impl Write,
  reader: &mut impl BufRead,
  commands: &[String],
) -> io::Result<()> {
  let mut dispatched = false;
  let writes = (|| {
    for command in commands {
      writer.write_all(command.as_bytes())?;
      dispatched = true;
    }
    writer.write_all(b"sync\n")?;
    writer.flush()
  })();
  // A helper that rejected an event can have closed stdin before we sent
  // its sync fence. Read its terminal response from the other pipe.
  if let Err(error) = writes
    && error.kind() != io::ErrorKind::BrokenPipe
  {
    return Err(if dispatched {
      ctream_driver::UnconfirmedDelivery::error(error)
    } else {
      error
    });
  }
  let error = match line(reader) {
    Ok(Some(line)) if line == "ok\n" => return Ok(()),
    Ok(Some(line)) if line == "refused\n" => return Err(io::Error::other("native input refused")),
    Ok(_) => io::Error::new(
      io::ErrorKind::UnexpectedEof,
      "native input acknowledgement missing or invalid",
    ),
    Err(error) => error,
  };
  Err(if dispatched {
    ctream_driver::UnconfirmedDelivery::error(error)
  } else {
    error
  })
}

#[cfg(any(target_os = "linux", windows))]
pub fn serve_driver(
  mode: Mode,
  mut gate: Gate,
  run: impl FnOnce(io::PipeReader, io::PipeWriter) -> ctream_driver::Result<()> + Send,
) -> ctream_driver::Result<()> {
  let (input_pipe, mut driver) = io::pipe()?;
  let (reader, output_pipe) = io::pipe()?;
  let mut completion = output_pipe.try_clone()?;
  let mut reader = io::BufReader::new(reader);
  let (incoming, requests) = mpsc::channel();
  let stopped = incoming.clone();
  std::thread::scope(|scope| -> io::Result<()> {
    let worker = scope.spawn(move || {
      let result = run(input_pipe, output_pipe).map_err(|error| {
        let error = match error.downcast::<io::Error>() {
          Ok(error) => *error,
          Err(error) => io::Error::other(error.to_string()),
        };
        if !ctream_driver::UnconfirmedDelivery::is(&error) {
          let _ = completion.write_all(b"refused\n");
        }
        error
      });
      drop(completion);
      let _ = stopped.send(Incoming::DriverEnded);
      result
    });
    let result = (|| -> io::Result<()> {
      let header =
        line(&mut reader)?.ok_or_else(|| io::Error::other("native input driver did not start"))?;
      // Test the backend fence before announcing readiness or delivering input.
      let mut send = |commands: &[String]| dispatch_native(&mut driver, &mut reader, commands);
      send(&[])?;
      let mut stdout = io::stdout().lock();
      stdout.write_all(header.as_bytes())?;
      stdout.flush()?;
      feed(mode, true, io::BufReader::new(io::stdin()), incoming)?;
      listen(&mut gate, requests, &mut stdout, send)
    })();
    drop(driver);
    // The driver releases held keys/buttons on EOF. Keep the admission until
    // that cleanup completes so releases cannot land inside a peer's gesture.
    let native = worker
      .join()
      .unwrap_or_else(|_| Err(io::Error::other("native input driver panicked")));
    let released = gate.finish();
    // A malformed frame already made delivery uncertain, even if the helper
    // subsequently reported an unrelated error during cleanup.
    if result
      .as_ref()
      .is_err_and(ctream_driver::UnconfirmedDelivery::is)
    {
      result
    } else {
      // Any worker error here happened before the first mutation, after its
      // acknowledgement, or after an explicit refusal. Retain its diagnostic.
      native
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
        .and(result)
        .and(released)
    }
  })?;
  Ok(())
}

#[cfg(test)]
mod delivery_tests {
  use super::*;

  #[test]
  fn only_a_lost_post_dispatch_ack_leaves_native_delivery_unconfirmed() {
    for (mutation, reply, unknown) in [
      (false, "", false),
      (false, "invalid\n", false),
      (true, "refused\n", false),
      (true, "invalid\n", true),
      (true, "ok", true),
      (true, "", true),
    ] {
      let commands = if mutation {
        vec!["key a 1\n".into()]
      } else {
        Vec::new()
      };
      let mut sent = Vec::new();
      let error = dispatch_native(&mut sent, &mut reply.as_bytes(), &commands).unwrap_err();
      assert_eq!(ctream_driver::UnconfirmedDelivery::is(&error), unknown);
      assert_eq!(sent.starts_with(b"key a 1\n"), mutation);
    }
  }
}
