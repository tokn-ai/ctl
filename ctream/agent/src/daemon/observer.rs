//! Observe several capture sources on one authenticated desktop-daemon connection.
use super::authority::{Command, Reply, exchange};
use super::transport::{self, Stream};
use super::{
  CONTROL_LIMIT, EVENT_LIMIT, IO_TIMEOUT, Opening, Room, invalid, read_json, write_json,
};
use ctream_client::sdk::Mutation;
use ctream_proto::{Target, live::LiveEvent, valid_agent_id};
use std::{collections::BTreeSet, io, path::Path};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const MAX_SOURCES: usize = 16;
// The private record wraps the ordinary event with a small source index.
const TAGGED_LIMIT: usize = EVENT_LIMIT + 16;

pub(super) fn validate(sources: &[Room], agent_id: &str) -> io::Result<()> {
  if !(1..=MAX_SOURCES).contains(&sources.len()) || !valid_agent_id(agent_id) {
    return Err(invalid(
      "expected a participant and 1 to 16 observed sources",
    ));
  }
  let mut seen = BTreeSet::new();
  for room in sources {
    room.validate()?;
    if !sources[0].shares_desktop(room) || !seen.insert(room) {
      return Err(invalid(
        "observed sources must be distinct and on the same desktop",
      ));
    }
  }
  Ok(())
}

/// A confirmed action, roster, or turn update and the source where it was observed.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEvent {
  pub target: Target,
  pub window_id: Option<String>,
  pub event: LiveEvent,
}

/// One desktop participant observing up to 16 sources. Each source starts with
/// its own roster and turn state. An optional control lane shares this identity.
pub struct SourceObserver {
  sources: Vec<(Target, Option<String>)>,
  stream: BufReader<Stream>,
  buffer: Vec<u8>,
  attachment: String,
}

impl SourceObserver {
  pub async fn connect(
    agent: &Path,
    agent_id: &str,
    sources: &[(Target, Option<String>)],
  ) -> io::Result<Self> {
    let rooms: Vec<_> = sources
      .iter()
      .map(|(target, window)| Room::new(*target, window.as_deref()))
      .collect();
    validate(&rooms, agent_id)?;
    let mut stream = BufReader::new(transport::connect(Some(agent), false).await?);
    write_json(
      stream.get_mut(),
      &Opening::ObserveSources {
        observe_sources: rooms,
        agent_id: agent_id.to_owned(),
      },
    )
    .await?;
    let attachment = stream
      .get_ref()
      .reply_deadline(IO_TIMEOUT)
      .wait(read_json::<String>(&mut stream, CONTROL_LIMIT))
      .await??
      .filter(|value| uuid::Uuid::try_parse(value).is_ok())
      .ok_or_else(|| invalid("desktop daemon rejected observed sources"))?;
    Ok(Self {
      sources: sources.to_owned(),
      stream,
      buffer: Vec::new(),
      attachment,
    })
  }

  /// Private bearer capability for a host transport pairing its observer and control streams.
  /// Deliver only to the creating client. The daemon revokes it on observer disconnect.
  pub fn attachment(&self) -> &str {
    &self.attachment
  }

  /// Attach one control lane to a currently observed source. The daemon revokes
  /// it when the event connection closes. The lane can run alongside next_event().
  pub async fn control(
    &self,
    target: Target,
    window_id: Option<&str>,
  ) -> io::Result<SourceControl> {
    if !self
      .sources
      .iter()
      .any(|(source, window)| *source == target && window.as_deref() == window_id)
    {
      return Err(invalid("control requires a subscribed source"));
    }
    let mut stream = BufReader::new(transport::connect(None, true).await?);
    write_json(
      stream.get_mut(),
      &Opening::AttachControl {
        attachment: self.attachment.clone(),
        source: Room::new(target, window_id),
      },
    )
    .await?;
    stream
      .get_ref()
      .reply_deadline(IO_TIMEOUT)
      .wait(read_json::<Reply>(&mut stream, CONTROL_LIMIT))
      .await??
      .ok_or_else(|| invalid("desktop daemon disconnected during attachment"))?
      .result()?;
    Ok(SourceControl {
      stream: Some(stream),
    })
  }

  /// Can be awaited in a select loop. A cancelled read retains partial record bytes.
  /// Reopening a disconnected observer obtains a fresh snapshot for every source.
  pub async fn next_event(&mut self) -> io::Result<Option<SourceEvent>> {
    let remaining = (TAGGED_LIMIT + 1).saturating_sub(self.buffer.len());
    let count = (&mut self.stream)
      .take(remaining as u64)
      .read_until(b'\n', &mut self.buffer)
      .await?;
    if count == 0 && self.buffer.is_empty() {
      return Ok(None);
    }
    if self.buffer.len() > TAGGED_LIMIT || self.buffer.last() != Some(&b'\n') {
      return Err(invalid("incomplete or oversized observed event"));
    }
    let (index, event): (usize, LiveEvent) = serde_json::from_slice(&self.buffer)?;
    self.buffer.clear();
    let (target, window) = self
      .sources
      .get(index)
      .ok_or_else(|| invalid("unknown observed source"))?;
    if !event.valid() {
      return Err(invalid("invalid observed event"));
    }
    Ok(Some(SourceEvent {
      target: *target,
      window_id: window.clone(),
      event,
    }))
  }
}

/// A source-specific input lane bound to the original participant's lifetime.
/// Dropping this lane releases its turn. Close it explicitly to await cleanup.
pub struct SourceControl {
  stream: Option<BufReader<Stream>>,
}

impl SourceControl {
  async fn request(&mut self, command: Command) -> io::Result<Option<String>> {
    // Cancellation, EOF or a lost reply must prevent reuse of the stream. A
    // response to an earlier command can never be mistaken for a new result.
    let mut stream = self
      .stream
      .take()
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "control lane disconnected"))?;
    match exchange(&mut stream, &command).await {
      Ok(reply) => {
        self.stream = Some(stream);
        reply.result()
      }
      Err(error) => {
        stream.get_ref().discard_pending();
        Err(if matches!(command, Command::Execute { .. }) {
          ctream_driver::UnconfirmedDelivery::error(error)
        } else {
          error
        })
      }
    }
  }

  pub async fn acquire(&mut self, ttl_ms: u64) -> io::Result<String> {
    self
      .request(Command::Acquire { ttl_ms })
      .await?
      .ok_or_else(|| invalid("missing turn ID"))
  }

  pub async fn execute(&mut self, turn_id: Option<&str>, action: Mutation) -> io::Result<String> {
    self
      .request(Command::Execute {
        turn_id: turn_id.map(str::to_owned),
        request: crate::desktop_input::InputRequest::Sdk { action },
      })
      .await?
      .ok_or_else(|| invalid("missing action ID"))
  }

  pub async fn release(&mut self, turn_id: &str) -> io::Result<()> {
    self
      .request(Command::Release {
        turn_id: turn_id.to_owned(),
      })
      .await
      .map(drop)
  }

  pub async fn close(mut self) -> io::Result<()> {
    self.request(Command::Close {}).await.map(drop)
  }
}
