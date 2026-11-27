//! The display host daemon, its platform workers and its local client protocol.
mod authority;
#[cfg(any(target_os = "linux", windows))]
mod capture;
mod server;
#[cfg(any(target_os = "linux", windows))]
pub use capture::stream as capture_stream;
mod endpoint;
mod observer;
pub use authority::{Authority, Delegate};
pub use observer::{SourceControl, SourceEvent, SourceObserver};
#[cfg(target_os = "linux")]
pub(crate) struct HelperHandles {
  pub(crate) registration: std::os::fd::OwnedFd,
  pub(crate) fence: std::fs::File,
}
pub use ctream_proto::live::TurnError;
#[cfg(windows)]
pub(crate) use endpoint::host_account_id;

use ctream_proto::{
  Target,
  live::{LiveAction, LiveEvent, Participant},
  valid_agent_id, valid_window_id,
};
use interprocess::local_socket::tokio::Stream;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
  io::{self, BufRead as _, Read, Write},
  thread,
  time::Duration,
};
use tokio::{
  io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
  runtime::{Builder, Runtime},
  sync::mpsc,
  time::timeout,
};

const CONTROL_LIMIT: usize = 4096;
const EVENT_LIMIT: usize = ctream_proto::live::RECORD_LIMIT;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Room {
  target: String,
  window: Option<String>,
}

impl Room {
  fn new(target: Target, window: Option<&str>) -> Self {
    Self {
      target: target.to_string(),
      window: window.map(str::to_owned),
    }
  }

  fn validate(&self) -> io::Result<()> {
    let target = Target::parse(&self.target).map_err(invalid)?;
    if target.to_string() != self.target
      || (target.window_only() && self.window.is_none())
      || self
        .window
        .as_deref()
        .is_some_and(|id| !valid_window_id(id))
    {
      return Err(invalid("invalid desktop source"));
    }
    Ok(())
  }
}

#[derive(Default, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
  #[default]
  Legacy,
  Turns,
  Observed,
  Authority,
}

impl Mode {
  fn legacy(&self) -> bool {
    *self == Self::Legacy
  }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum Opening {
  #[cfg(any(target_os = "linux", windows))]
  Capture {
    source: capture::SourceKey,
  },
  ObserveSources {
    observe_sources: Vec<Room>,
    agent_id: String,
  },
  AttachControl {
    attachment: String,
    source: Room,
  },
  Join(Join),
  Delegate {
    room: Room,
    delegation_key: String,
  },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Join {
  room: Room,
  participant: Participant,
  subscribe: bool,
  #[serde(default, skip_serializing_if = "Mode::legacy")]
  mode: Mode,
}

impl Join {
  fn new(
    target: Target,
    window: Option<&str>,
    agent_id: &str,
    session_id: &str,
    subscribe: bool,
    mode: Mode,
  ) -> io::Result<Self> {
    let join = Self {
      room: Room::new(target, window),
      participant: Participant {
        agent_id: agent_id.into(),
        participant_id: session_id.into(),
      },
      subscribe,
      mode,
    };
    join.validate()?;
    Ok(join)
  }
  fn validate(&self) -> io::Result<()> {
    self.room.validate()?;
    if !valid_agent_id(&self.participant.agent_id)
      || (matches!(self.mode, Mode::Turns | Mode::Observed) && !self.subscribe)
      || (self.mode == Mode::Authority && self.subscribe)
      || !uuid::Uuid::try_parse(&self.participant.participant_id)
        .is_ok_and(|id| id.to_string() == self.participant.participant_id)
    {
      return Err(invalid("invalid live participant or source"));
    }
    Ok(())
  }
}

fn invalid(message: &'static str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}

fn runtime() -> io::Result<Runtime> {
  Builder::new_current_thread().enable_all().build()
}

async fn read_json<T: DeserializeOwned>(
  reader: &mut (impl AsyncBufRead + Unpin),
  limit: usize,
) -> io::Result<Option<T>> {
  let mut bytes = Vec::new();
  if reader
    .take((limit + 1) as u64)
    .read_until(b'\n', &mut bytes)
    .await?
    == 0
  {
    return Ok(None);
  }
  if bytes.last() != Some(&b'\n') {
    return Err(invalid("incomplete or oversized live event"));
  }
  Ok(Some(serde_json::from_slice(&bytes)?))
}

async fn write_json<T: Serialize>(
  writer: &mut (impl AsyncWrite + Unpin),
  value: &T,
) -> io::Result<()> {
  let mut bytes = serde_json::to_vec(value)?;
  bytes.push(b'\n');
  timeout(IO_TIMEOUT, writer.write_all(&bytes)).await??;
  Ok(())
}

async fn join(request: &Join) -> io::Result<BufReader<Stream>> {
  let mut stream = BufReader::new(endpoint::connect().await?);
  write_json(stream.get_mut(), request).await?;
  let admitted: Option<bool> = timeout(IO_TIMEOUT, read_json(&mut stream, 16)).await??;
  if admitted != Some(true) {
    return Err(io::Error::other("desktop daemon declined registration"));
  }
  Ok(stream)
}

/// Start the user-local daemon if needed. Does not join a room.
pub fn ensure_daemon() -> io::Result<()> {
  runtime()?.block_on(endpoint::connect()).map(drop)
}

/// Entry point for the independently launched, idle-exiting host process.
pub fn run_daemon() -> io::Result<()> {
  #[cfg(unix)]
  if rustix::process::getpgrp() != rustix::process::getpid() {
    rustix::process::setsid()?;
  }
  runtime()?.block_on(server::serve())
}

/// An SDK session advertises its presence without subscribing to the event stream.
pub struct Publisher {
  runtime: Runtime,
  stream: Option<BufReader<Stream>>,
}

impl Publisher {
  pub fn connect(
    target: Target,
    window: Option<&str>,
    agent_id: &str,
    session_id: &str,
  ) -> io::Result<Self> {
    let runtime = runtime()?;
    let request = Join::new(target, window, agent_id, session_id, false, Mode::Legacy)?;
    let stream = runtime.block_on(join(&request))?;
    Ok(Self {
      runtime,
      stream: Some(stream),
    })
  }

  pub fn publish(&mut self, action: &LiveAction) -> io::Result<()> {
    if !action.valid() {
      return Err(invalid("invalid normalized live action"));
    }
    let mut stream = self
      .stream
      .take()
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "live publisher disconnected"))?;
    let result = self.runtime.block_on(async {
      // The caller holds the host action lock. Bound the round trip while
      // ordering events from independent SDK clients at the daemon.
      timeout(Duration::from_millis(400), async {
        write_json(stream.get_mut(), action).await?;
        match read_json::<bool>(&mut stream, 16).await? {
          Some(true) => Ok(()),
          _ => Err(invalid("live publication was not acknowledged")),
        }
      })
      .await?
    });
    if result.is_ok() {
      self.stream = Some(stream);
    } else {
      endpoint::discard_pending(stream.get_ref());
    }
    result
  }
}

/// Relay a participant's NDJSON actions and host events. Closing input leaves the room.
pub fn serve_peer(
  target: Target,
  window: Option<&str>,
  agent_id: &str,
  session_id: &str,
  input: impl Read + Send + 'static,
  output: impl Write,
) -> io::Result<()> {
  serve_mode(
    Join::new(target, window, agent_id, session_id, true, Mode::Legacy)?,
    input,
    output,
  )
}

pub fn serve_turn_peer(
  target: Target,
  window: Option<&str>,
  agent_id: &str,
  session_id: &str,
  input: impl Read + Send + 'static,
  output: impl Write,
) -> io::Result<()> {
  serve_mode(
    Join::new(target, window, agent_id, session_id, true, Mode::Turns)?,
    input,
    output,
  )
}

pub fn serve_observer(
  target: Target,
  window: Option<&str>,
  agent_id: &str,
  session_id: &str,
  input: impl Read + Send + 'static,
  output: impl Write,
) -> io::Result<()> {
  serve_mode(
    Join::new(target, window, agent_id, session_id, true, Mode::Observed)?,
    input,
    output,
  )
}

fn serve_mode(
  request: Join,
  input: impl Read + Send + 'static,
  mut output: impl Write,
) -> io::Result<()> {
  let runtime = runtime()?;
  runtime.block_on(async {
    let mut stream = join(&request).await?;
    let snapshot: LiveEvent = timeout(IO_TIMEOUT, read_json(&mut stream, EVENT_LIMIT))
      .await??
      .ok_or_else(|| invalid("desktop daemon ended before the roster"))?;
    let mut deliver = |event: &LiveEvent| -> io::Result<()> {
      serde_json::to_writer(&mut output, event)?;
      output.write_all(b"\n")?;
      output.flush()
    };
    deliver(&snapshot)?;
    let (received, mut requests) = mpsc::channel(8);
    thread::Builder::new()
      .name("ctream-room-stdin".into())
      .spawn(move || {
        let mut input = io::BufReader::new(input);
        loop {
          let mut bytes = Vec::new();
          let result = match input
            .by_ref()
            .take((EVENT_LIMIT + 1) as u64)
            .read_until(b'\n', &mut bytes)
          {
            Ok(0) => break,
            Ok(_) if bytes.last() == Some(&b'\n') => {
              serde_json::from_slice::<LiveAction>(&bytes).map_err(io::Error::from)
            }
            Ok(_) => Err(invalid("incomplete or oversized live action")),
            Err(error) => Err(error),
          };
          let failed = result.is_err();
          if received.blocking_send(result).is_err() || failed {
            break;
          }
        }
      })?;
    // The handshake reader can already contain an event following the roster.
    // Splitting this buffered stream keeps those bytes in the incoming half.
    let (read, mut write) = tokio::io::split(stream);
    let send = async {
      while let Some(action) = requests.recv().await {
        let action = action?;
        if !action.valid() {
          return Err(invalid("invalid normalized live action"));
        }
        write_json(&mut write, &action).await?;
      }
      Ok(())
    };
    let receive = async {
      let mut reader = BufReader::new(read);
      while let Some(event) = read_json::<LiveEvent>(&mut reader, EVENT_LIMIT).await? {
        deliver(&event)?;
      }
      Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "desktop daemon disconnected",
      ))
    };
    tokio::select! { result = send => result, result = receive => result }
  })
}

#[cfg(target_os = "macos")]
pub(crate) fn picker_directory() -> io::Result<std::path::PathBuf> {
  endpoint::picker_directory()
}
