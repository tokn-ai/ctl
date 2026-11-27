//! Host-only capabilities for a controller and its delegated native driver.
use super::{
  CONTROL_LIMIT, IO_TIMEOUT, Join, Mode, Opening, Room, endpoint, invalid, read_json, runtime,
  write_json,
};
use ctream_proto::{
  Target,
  live::{ActionFailure, TurnError},
};
use interprocess::local_socket::tokio::Stream;
use serde::{Deserialize, Serialize};
use std::{fs::File, io, path::Path, time::Duration};
use tokio::{io::BufReader, runtime::Runtime, time::timeout};

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Command {
  Acquire {
    ttl_ms: u64,
  },
  Release {
    turn_id: String,
  },
  Begin {
    turn_id: Option<String>,
  },
  Finish {},
  PrepareRaw {
    picker: Option<String>,
    lease: Option<String>,
  },
  Execute {
    turn_id: Option<String>,
    request: crate::desktop_input::InputRequest,
  },
  Close {},
}
// Bound wire diagnostics independently of how much stderr a platform worker
// produced. A long error must not hide the action ID or its delivery outcome.
fn diagnostic(message: &str) -> String {
  message
    .chars()
    .take(512)
    .map(|c| if c.is_control() { ' ' } else { c })
    .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Failure {
  Conflict(TurnError),
  Refused(String),
  Unknown(String),
  Driver(String),
}
impl Failure {
  fn into_error(self) -> io::Error {
    match self {
      Self::Conflict(conflict) => io::Error::new(io::ErrorKind::WouldBlock, conflict),
      Self::Refused(message) => io::Error::other(ctream_client::sdk::SemanticRefusal(message)),
      Self::Unknown(message) => {
        ctream_driver::UnconfirmedDelivery::error(io::Error::other(message))
      }
      Self::Driver(message) => io::Error::other(message),
    }
  }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Reply {
  Ok {
    value: Option<String>,
  },
  Conflict {
    code: String,
    owner_id: Option<String>,
  },
  ActionFailed {
    action_id: String,
    failure: Failure,
  },
  Pending {
    owner_id: String,
  },
  Invalid {
    message: String,
  },
}
impl Reply {
  pub(super) fn done() -> Self {
    Self::Ok { value: None }
  }
  pub(super) fn value(s: String) -> Self {
    Self::Ok { value: Some(s) }
  }
  pub(super) fn invalid(s: &str) -> Self {
    Self::Invalid {
      message: diagnostic(s),
    }
  }
  pub(super) fn action_error(action_id: String, error: io::Error) -> Self {
    let message = diagnostic(&error.to_string());
    let failure = if ctream_driver::UnconfirmedDelivery::is(&error) {
      Failure::Unknown(message)
    } else if error
      .get_ref()
      .is_some_and(|e| e.is::<ctream_client::sdk::SemanticRefusal>())
    {
      Failure::Refused(message)
    } else {
      Failure::Driver(message)
    };
    Self::ActionFailed { action_id, failure }
  }

  pub(super) fn result(self) -> io::Result<Option<String>> {
    match self {
      Self::Ok { value } => Ok(value),
      Self::Conflict { code, owner_id } => Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        TurnError { code, owner_id },
      )),
      Self::ActionFailed { action_id, failure } => {
        let source = failure.into_error();
        Err(io::Error::new(
          source.kind(),
          ActionFailure { action_id, source },
        ))
      }
      Self::Pending { owner_id } => {
        Err(io::Error::new(io::ErrorKind::WouldBlock, Pending(owner_id)))
      }
      Self::Invalid { message } => Err(io::Error::new(io::ErrorKind::InvalidInput, message)),
    }
  }
}

#[derive(Debug)]
struct Pending(String);
impl std::fmt::Display for Pending {
  fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    out.write_str("previous gesture is finishing")
  }
}
impl std::error::Error for Pending {}

struct Connection {
  runtime: Runtime,
  stream: Option<BufReader<Stream>>,
  // Held locally across driver execution, independently of the daemon process.
  // A new daemon cannot admit anyone until previous-generation gestures drain.
  fence: Option<File>,
  #[cfg(target_os = "linux")]
  helper_fence: Option<File>,
  failed: bool,
}

impl Connection {
  fn open(opening: Opening, executable: Option<&Path>) -> io::Result<(Self, Option<String>)> {
    let runtime = runtime()?;
    let (stream, value) = runtime.block_on(async {
      let stream = match opening {
        Opening::Delegate { .. } | Opening::AttachControl { .. } => {
          endpoint::connect_existing().await?
        }
        _ => endpoint::connect_via(executable).await?,
      };
      let mut stream = BufReader::new(stream);
      write_json(stream.get_mut(), &opening).await?;
      let reply: Reply = timeout(IO_TIMEOUT, read_json(&mut stream, CONTROL_LIMIT))
        .await??
        .ok_or_else(|| {
          io::Error::new(io::ErrorKind::UnexpectedEof, "turn authority unavailable")
        })?;
      Ok::<_, io::Error>((stream, reply.result()?))
    })?;
    Ok((
      Self {
        runtime,
        stream: Some(stream),
        fence: None,
        #[cfg(target_os = "linux")]
        helper_fence: None,
        failed: false,
      },
      value,
    ))
  }
  fn request(&mut self, command: Command) -> io::Result<Option<String>> {
    if self.failed {
      return Err(io::Error::new(
        io::ErrorKind::NotConnected,
        "turn authority disconnected",
      ));
    }
    let mut stream = self
      .stream
      .take()
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "turn authority disconnected"))?;
    // Platform workers have their own startup/delivery timeouts. Never retry an
    // execute after losing its reply: the journal may already contain delivery.
    let execution = matches!(command, Command::Execute { .. });
    let result = self.runtime.block_on(exchange(&mut stream, &command));
    match result {
      Ok(reply) => {
        self.stream = Some(stream);
        reply.result()
      }
      Err(err) => {
        // Keep the registered socket alive while physical execution drains,
        // even if a response timed out. A desynchronized stream is never reused.
        endpoint::discard_pending(stream.get_ref());
        self.failed = true;
        if self.fence.is_some() {
          self.stream = Some(stream);
        }
        Err(if execution {
          ctream_driver::UnconfirmedDelivery::error(err)
        } else {
          err
        })
      }
    }
  }
  fn begin(&mut self, turn_id: Option<&str>) -> io::Result<String> {
    if self.fence.is_some() {
      return Err(invalid("gesture already active"));
    }
    #[cfg(target_os = "linux")]
    let fence = match &self.helper_fence {
      Some(file) => {
        let fence = file.try_clone()?;
        fence.try_lock_shared()?;
        fence
      }
      None => endpoint::gesture_fence()?,
    };
    #[cfg(not(target_os = "linux"))]
    let fence = endpoint::gesture_fence()?;
    let result = self.admit(turn_id);
    if result.is_ok() {
      self.fence = Some(fence);
    } else {
      fence.unlock()?;
    }
    result
  }

  #[cfg(target_os = "linux")]
  fn helper_handles(&mut self) -> io::Result<super::HelperHandles> {
    use std::os::fd::AsFd;
    let stream = self
      .stream
      .as_ref()
      .ok_or_else(|| invalid("controller disconnected"))?;
    let Stream::UdSocket(socket) = stream.get_ref();
    if self.helper_fence.is_none() {
      if self.fence.is_some() {
        return Err(invalid("register helper before admitting input"));
      }
      self.helper_fence = Some(endpoint::fence_file()?);
    }
    Ok(super::HelperHandles {
      registration: socket.as_fd().try_clone_to_owned()?,
      fence: self.helper_fence.as_ref().unwrap().try_clone()?,
    })
  }

  fn admit(&mut self, turn_id: Option<&str>) -> io::Result<String> {
    let end = std::time::Instant::now() + IO_TIMEOUT;
    loop {
      match self.request(Command::Begin {
        turn_id: turn_id.map(str::to_owned),
      }) {
        Err(error) => {
          let Some(wait) = error
            .get_ref()
            .and_then(|cause| cause.downcast_ref::<Pending>())
          else {
            return Err(error);
          };
          if std::time::Instant::now() >= end {
            return Err(io::Error::new(
              io::ErrorKind::WouldBlock,
              TurnError {
                code: "busy".into(),
                owner_id: Some(wait.0.clone()),
              },
            ));
          }
          std::thread::sleep(Duration::from_millis(10));
        }
        Ok(reply) => return reply.ok_or_else(|| invalid("missing admission ID")),
      }
    }
  }
  fn finish(&mut self) -> io::Result<()> {
    if self.fence.is_none() {
      return Err(invalid("no gesture is active"));
    }
    let result = self.request(Command::Finish {}).map(drop);
    if self.failed {
      self.stream = None
    }
    self.fence.take().expect("active gesture").unlock()?;
    result
  }
}

pub(super) async fn exchange(
  stream: &mut BufReader<Stream>,
  command: &Command,
) -> io::Result<Reply> {
  let deadline = if matches!(
    command,
    Command::Execute { .. } | Command::PrepareRaw { .. } | Command::Close {}
  ) {
    Duration::from_secs(60)
  } else {
    IO_TIMEOUT
  };
  timeout(deadline, async {
    write_json(stream.get_mut(), command).await?;
    read_json::<Reply>(stream, CONTROL_LIMIT)
      .await?
      .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "turn authority disconnected"))
  })
  .await?
}

/// One host controller. The daemon binds turn tokens to this connection and
/// gates each complete physical or accessibility gesture on its desktop.
pub struct Authority {
  connection: Connection,
  #[cfg(test)]
  key: String,
}
impl Authority {
  /// Pair an existing observer with a control stream for one of its exact sources.
  pub fn attach(target: Target, window: Option<&str>, secret: &str) -> io::Result<Self> {
    if uuid::Uuid::try_parse(secret).is_err() {
      return Err(invalid("invalid observer attachment"));
    }
    let (connection, _reply) = Connection::open(
      Opening::AttachControl {
        attachment: secret.to_owned(),
        source: Room::new(target, window),
      },
      None,
    )?;
    Ok(Self {
      connection,
      #[cfg(test)]
      key: _reply.unwrap_or_default(),
    })
  }

  pub fn connect(
    target: Target,
    window: Option<&str>,
    agent_id: &str,
    participant_id: &str,
  ) -> io::Result<Self> {
    Self::open(
      None,
      target,
      window,
      agent_id,
      participant_id,
      Mode::Authority,
    )
  }
  pub fn connect_via(
    agent: &Path,
    target: Target,
    window: Option<&str>,
    agent_id: &str,
    participant_id: &str,
  ) -> io::Result<Self> {
    Self::open(
      Some(agent),
      target,
      window,
      agent_id,
      participant_id,
      Mode::Authority,
    )
  }
  fn open(
    agent: Option<&Path>,
    target: Target,
    window: Option<&str>,
    agent_id: &str,
    participant_id: &str,
    mode: Mode,
  ) -> io::Result<Self> {
    let join = Join::new(target, window, agent_id, participant_id, false, mode)?;
    let (connection, _reply) = Connection::open(Opening::Join(join), agent)?;
    #[cfg(not(test))]
    if _reply.is_some() {
      return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the running host service does not support daemon input dispatch",
      ));
    }
    Ok(Self {
      connection,
      #[cfg(test)]
      key: _reply.unwrap_or_default(),
    })
  }
  #[cfg(test)]
  pub fn delegation_key(&self) -> &str {
    &self.key
  }
  pub fn acquire(&mut self, ttl_ms: u64) -> io::Result<String> {
    self
      .connection
      .request(Command::Acquire { ttl_ms })?
      .ok_or_else(|| invalid("missing turn ID"))
  }
  pub fn release(&mut self, turn_id: &str) -> io::Result<()> {
    self
      .connection
      .request(Command::Release {
        turn_id: turn_id.into(),
      })
      .map(drop)
  }
  #[cfg(test)]
  pub fn begin(&mut self, turn_id: Option<&str>) -> io::Result<String> {
    self.connection.begin(turn_id)
  }
  #[cfg(test)]
  pub fn finish(&mut self) -> io::Result<()> {
    self.connection.finish()
  }

  pub fn execute(
    &mut self,
    turn_id: Option<&str>,
    request: crate::desktop_input::InputRequest,
  ) -> io::Result<String> {
    self
      .connection
      .request(Command::Execute {
        turn_id: turn_id.map(str::to_owned),
        request,
      })?
      .ok_or_else(|| invalid("missing action ID"))
  }

  pub fn prepare_raw(&mut self, picker: Option<&str>, lease: Option<&str>) -> io::Result<String> {
    self
      .connection
      .request(Command::PrepareRaw {
        picker: picker.map(str::to_owned),
        lease: lease.map(str::to_owned),
      })?
      .ok_or_else(|| invalid("missing native input header"))
  }

  pub fn close(&mut self) -> io::Result<()> {
    self.connection.request(Command::Close {}).map(drop)
  }
}

/// A host-local driver connection pinned to an authority's selected source.
/// Hold a pin from before the first event until the driver has applied its batch
/// (and released held input on cancellation). It survives parent disconnect/TTL.
pub struct Delegate(Connection);
impl Delegate {
  #[cfg(target_os = "linux")]
  pub(crate) fn helper_handles(&mut self) -> io::Result<super::HelperHandles> {
    self.0.helper_handles()
  }
  pub fn connect(target: Target, window: Option<&str>, key: &str) -> io::Result<Self> {
    let room = Room::new(target, window);
    let (connection, _) = Connection::open(
      Opening::Delegate {
        room,
        delegation_key: key.to_owned(),
      },
      None,
    )?;
    Ok(Self(connection))
  }
  pub fn begin(&mut self) -> io::Result<()> {
    self.0.begin(None).map(drop)
  }
  pub fn finish(&mut self) -> io::Result<()> {
    self.0.finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_long_driver_diagnostic_preserves_its_receipt_and_uncertain_outcome() {
    let id = uuid::Uuid::new_v4().to_string();
    let error = ctream_driver::UnconfirmedDelivery::error(io::Error::other("🖥\n".repeat(4096)));
    let wire = serde_json::to_vec(&Reply::action_error(id.clone(), error)).unwrap();
    assert!(wire.len() < CONTROL_LIMIT);
    let failure = serde_json::from_slice::<Reply>(&wire)
      .unwrap()
      .result()
      .unwrap_err();
    let failure = failure
      .get_ref()
      .unwrap()
      .downcast_ref::<ActionFailure>()
      .unwrap();
    assert_eq!(failure.action_id, id);
    assert!(ctream_driver::UnconfirmedDelivery::is(&failure.source));
  }
}
