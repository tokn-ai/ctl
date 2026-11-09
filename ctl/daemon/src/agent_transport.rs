//! Session and UDP service pools share an authenticated master independently.

#[cfg(all(test, unix))]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use ctl_ipc::SshTarget;
use ctl_proto::transport::{self, Client, Service, Stream};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, BufReader, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex as AsyncMutex, watch};

use super::ssh_process::Process;
use super::{RequestError, SSH_PROGRAM, State, append_target_arguments, control_master_is_ready};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_STARTUP_BYTES: usize = 64 * 1024;
const MAX_COMPONENT_BYTES: usize = 16 * 1024;
const STARTUP_MARKER: &[u8] = b"ctl-multiplex-startup\n";
const MISSING_AGENT_MARKER: &[u8] = b"ctl-multiplex-missing-agent\n";
const UNSUPPORTED_AGENT_MARKER: &[u8] = b"ctl-multiplex-unsupported-agent\n";
const REMOTE_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/bin:$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-multiplex-missing-agent\n'; exit 127; }; "#,
  "printf 'ctl-multiplex-startup\\n'; ",
  r#"if component_info=$(ctl-agent --component-info); then printf '%s\n' "$component_info"; "#,
  "exec ctl-agent multiplex; else printf 'ctl-multiplex-unsupported-agent\\n'; exit 1; fi",
);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct MissingAgent(String);

pub(super) fn agent_is_missing(error: &io::Error) -> bool {
  let Some(source) = error.get_ref() else {
    return false;
  };
  source.is::<MissingAgent>()
}

fn unsupported_agent() -> io::Error {
  io::Error::new(
    io::ErrorKind::Unsupported,
    "Shared SSH streams require ctl-agent with ctl_transport 1.0.1. Update the remote components, then reconnect.",
  )
}

#[derive(Default)]
struct GatewaySlot {
  initialization: AsyncMutex<()>,
  owned: Mutex<OwnedGateway>,
}

#[derive(Default)]
struct OwnedGateway {
  targets: HashMap<SshTarget, watch::Sender<bool>>,
  client: Option<Client>,
}

#[derive(Default)]
pub(super) struct Registry {
  gateways: Mutex<HashMap<PathBuf, MasterGateway>>,
}

struct MasterGateway {
  socket_identity: (u64, u64),
  groups: HashMap<Group, Arc<GatewaySlot>>,
}

struct Reservation {
  target: SshTarget,
  control_path: PathBuf,
  socket_identity: (u64, u64),
  service: Service,
  group: Group,
  slot: Arc<GatewaySlot>,
  cancelled: watch::Receiver<bool>,
}

impl MasterGateway {
  fn close(self) {
    for slot in self.groups.into_values() {
      let mut owned = slot.owned.lock().unwrap();
      for (_, cancelled) in owned.targets.drain() {
        cancelled.send_replace(true);
      }
      if let Some(client) = owned.client.take() {
        client.close();
      }
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Group {
  Sessions,
  Forwarding,
}

impl Group {
  fn for_service(service: Service) -> Self {
    match service {
      Service::Ctmux | Service::Task => Self::Sessions,
      Service::Udp => Self::Forwarding,
    }
  }

  fn capacity_error(self, error: io::Error) -> io::Error {
    if error.kind() != io::ErrorKind::WouldBlock {
      return error;
    }
    let (name, recovery) = match self {
      Self::Sessions => ("sessions", "Close an unused terminal view"),
      Self::Forwarding => (
        "UDP forwarding",
        "Stop an unused UDP or Any forwarding rule",
      ),
    };
    io::Error::new(
      io::ErrorKind::WouldBlock,
      format!(
        "The shared SSH {name} pool has reached its limit of {} open streams. {recovery}, then try again.",
        transport::MAX_STREAMS,
      ),
    )
  }
}

pub(super) async fn open_service(
  local: &mut ctl_ipc::Stream,
  state: &Arc<State>,
  target: &SshTarget,
  control_path: &Path,
  service: Service,
) -> Result<(), RequestError> {
  super::validate_target(target)?;
  #[cfg(unix)]
  let monitor = super::client_lifetime::Monitor::new(local).map_err(RequestError::StartMaster)?;
  let startup = async {
    let lifecycle = state.target(target);
    let mut attempt = lifecycle.attempt();
    let target_guard = attempt
      .run(Arc::clone(&lifecycle.lock).lock_owned())
      .await?;
    lifecycle.require_connected()?;
    let endpoint = state
      .existing_endpoint(target)?
      .ok_or(RequestError::HostDisconnected)?;
    if endpoint.control_path != control_path {
      return Err(RequestError::InvalidRequest(
        "the selected SSH master changed before opening the service",
      ));
    }
    if !attempt
      .run(control_master_is_ready(target, control_path))
      .await??
    {
      return Err(RequestError::HostDisconnected);
    }
    // Register ownership before releasing the target lock. Disconnect may
    // already have published cancellation, but its registry cleanup must run
    // after this reservation and cannot miss a later stale registration.
    let reserved = state
      .agent_transports
      .reserve(target, control_path, service)
      .map_err(RequestError::AgentTransport)?;
    // Remote startup remains cancellable without holding the other class
    // behind this target's lifecycle lock.
    drop(target_guard);
    let remote = attempt
      .run(state.agent_transports.open_reserved(reserved))
      .await?
      .map_err(RequestError::AgentTransport)?;
    Ok::<_, RequestError>((remote, attempt))
  };
  #[cfg(unix)]
  let (mut remote, mut attempt) = tokio::select! {
    result = startup => result?,
    _ = monitor.closed() => return Err(RequestError::ClientClosed),
  };
  #[cfg(not(unix))]
  let (mut remote, mut attempt) = startup.await?;
  relay_service(local, &mut remote, &mut attempt).await
}

async fn relay_service(
  local: &mut ctl_ipc::Stream,
  remote: &mut (impl AsyncRead + AsyncWrite + Unpin),
  attempt: &mut super::target_lifecycle::TargetAttempt,
) -> Result<(), RequestError> {
  ctl_ipc::write_frame(local, &ctl_ipc::ServerMessage::ServiceOpened).await?;
  // EOF closes only this logical service. Its agent handler releases the
  // daemon attachment, while the shared SSH connection keeps serving siblings.
  attempt
    .run(tokio::io::copy_bidirectional(local, remote))
    .await
    .map_err(|error| RequestError::ServiceStreamClosed(io::Error::other(error)))?
    .map_err(RequestError::ServiceStreamClosed)?;
  Ok(())
}

impl Registry {
  pub(super) async fn open(
    &self,
    target: &SshTarget,
    control_path: &Path,
    service: Service,
  ) -> io::Result<Stream> {
    self
      .open_with(target, control_path, service, |selected_path| {
        start(command(target, &selected_path))
      })
      .await
  }

  async fn open_with<F, Fut>(
    &self,
    target: &SshTarget,
    control_path: &Path,
    service: Service,
    connect: F,
  ) -> io::Result<Stream>
  where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = io::Result<Client>>,
  {
    self
      .open_reserved_with(self.reserve(target, control_path, service)?, connect)
      .await
  }

  fn reserve(
    &self,
    target: &SshTarget,
    control_path: &Path,
    service: Service,
  ) -> io::Result<Reservation> {
    // Aliases can resolve to the same master. Canonicalize symlinks as well so
    // the authenticated control socket, rather than a target spelling, owns it.
    let control_path = std::fs::canonicalize(control_path)?;
    let group = Group::for_service(service);
    let (slot, cancelled, socket_identity) = {
      let mut gateways = self.gateways.lock().unwrap();
      // Observe identity while holding the registry lock: an opener that read
      // an older inode must not replace a newer entry another opener installed.
      let socket_identity = socket_identity(&control_path)?;
      if gateways
        .get(&control_path)
        .is_some_and(|master| master.socket_identity != socket_identity)
        && let Some(previous) = gateways.remove(&control_path)
      {
        // Both groups belong to this physical master, even when one has only
        // a pending startup and no published client to inspect yet.
        previous.close();
      }
      let master = gateways
        .entry(control_path.clone())
        .or_insert_with(|| MasterGateway {
          socket_identity,
          groups: HashMap::new(),
        });
      let slot = master.groups.entry(group).or_default();
      let cancelled = slot
        .owned
        .lock()
        .unwrap()
        .targets
        .entry(target.clone())
        .or_insert_with(|| watch::channel(false).0)
        .subscribe();
      (Arc::clone(slot), cancelled, socket_identity)
    };
    Ok(Reservation {
      target: target.clone(),
      control_path,
      socket_identity,
      service,
      group,
      slot,
      cancelled,
    })
  }

  async fn open_reserved(&self, reserved: Reservation) -> io::Result<Stream> {
    let target = reserved.target.clone();
    self
      .open_reserved_with(reserved, |selected_path| {
        start(command(&target, &selected_path))
      })
      .await
  }

  async fn open_reserved_with<F, Fut>(
    &self,
    reserved: Reservation,
    connect: F,
  ) -> io::Result<Stream>
  where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = io::Result<Client>>,
  {
    let Reservation {
      target,
      control_path,
      socket_identity,
      service,
      group,
      slot,
      mut cancelled,
    } = reserved;
    let availability = cancelled.clone();
    let open = async {
      let client = {
        // Ownership uses a separate short lock. Disconnect never waits for
        // remote startup, including another alias initializing this socket.
        let _initializing = slot.initialization.lock().await;
        self.require_current_master(&control_path, socket_identity)?;
        let existing = {
          let mut owned = slot.owned.lock().unwrap();
          if owned.client.as_ref().is_some_and(Client::is_closed)
            && let Some(previous) = owned.client.take()
          {
            previous.close();
            // Only this group's agent closed. Its old aliases must retry,
            // while the other group retains its client and ownership.
            owned.targets.retain(|owner, cancelled| {
              if owner == &target {
                true
              } else {
                cancelled.send_replace(true);
                false
              }
            });
          }
          owned.client.clone()
        };
        if let Some(client) = existing {
          client
        } else {
          let client = connect(control_path.clone()).await?;
          if let Err(error) = self.require_current_master(&control_path, socket_identity) {
            client.close();
            return Err(error);
          }
          let mut owned = slot.owned.lock().unwrap();
          if *availability.borrow() {
            // A cancelled initializer may finish in the same poll as its
            // disconnect. Never publish a client into a detached old slot.
            client.close();
            return Err(cancelled_open());
          }
          owned.client = Some(client.clone());
          client
        }
      };
      client
        .open(service)
        .await
        .map_err(|error| group.capacity_error(error))
    };
    tokio::select! {
      biased;
      _ = cancelled.wait_for(|cancelled| *cancelled) => Err(cancelled_open()),
      result = open => result,
    }
  }

  pub(super) fn cancel_target(&self, target: &SshTarget) {
    self.gateways.lock().unwrap().retain(|_, master| {
      master.groups.retain(|_, slot| {
        let mut owned = slot.owned.lock().unwrap();
        if let Some(cancelled) = owned.targets.remove(target) {
          cancelled.send_replace(true);
        }
        if !owned.targets.is_empty() {
          return true;
        }
        if let Some(client) = owned.client.take() {
          client.close();
        }
        false
      });
      !master.groups.is_empty()
    });
  }

  pub(super) fn shutdown(&self) {
    let masters: Vec<_> = self
      .gateways
      .lock()
      .unwrap()
      .drain()
      .map(|(_, master)| master)
      .collect();
    for master in masters {
      master.close();
    }
  }

  fn require_current_master(&self, path: &Path, expected: (u64, u64)) -> io::Result<()> {
    if socket_identity(path)? == expected {
      return Ok(());
    }
    let mut gateways = self.gateways.lock().unwrap();
    if gateways
      .get(path)
      .is_some_and(|master| master.socket_identity == expected)
      && let Some(previous) = gateways.remove(path)
    {
      previous.close();
    }
    Err(io::Error::new(
      io::ErrorKind::ConnectionAborted,
      "The selected SSH master changed while opening its shared service stream",
    ))
  }
}

fn cancelled_open() -> io::Error {
  io::Error::new(
    io::ErrorKind::ConnectionAborted,
    "The target disconnected while opening its shared service stream",
  )
}

fn socket_identity(path: &Path) -> io::Result<(u64, u64)> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
  }
  #[cfg(not(unix))]
  {
    let _ = path;
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "shared agent transport requires an SSH control master",
    ))
  }
}

fn command(target: &SshTarget, control_path: &Path) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command
    .args(["-T", "-S"])
    .arg(control_path)
    .args(["-o", "ControlMaster=no", "-o", "ProxyCommand=false"])
    .args(["-o", "BatchMode=yes", "-o", "StdinNull=no"])
    .args(["-o", "SessionType=default"])
    .args([
      "-o",
      "ForkAfterAuthentication=no",
      "-o",
      "RemoteCommand=none",
    ])
    .args(["-o", "ClearAllForwardings=yes", "-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no", "-o", "PermitLocalCommand=no"])
    .args(["-o", "ConnectTimeout=10", "-o", "ConnectionAttempts=1"])
    .args([
      "-o",
      "ServerAliveInterval=10",
      "-o",
      "ServerAliveCountMax=3",
    ])
    .env("SSH_ASKPASS_REQUIRE", "never");
  append_target_arguments(&mut command, target);
  command
    .arg(REMOTE_COMMAND)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  command
}

struct GatewayIo {
  process: Process,
  stdin: ChildStdin,
  stdout: BufReader<ChildStdout>,
}

impl AsyncRead for GatewayIo {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stdout).poll_read(cx, bytes)
  }
}

impl AsyncWrite for GatewayIo {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    Pin::new(&mut self.stdin).poll_write(cx, bytes)
  }

  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stdin).poll_flush(cx)
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stdin).poll_shutdown(cx)
  }
}

async fn start(command: Command) -> io::Result<Client> {
  let mut process = Process::spawn(command)?;
  let stdin = process.stdin().expect("agent transport stdin is piped");
  let stdout = BufReader::new(process.stdout().expect("agent transport stdout is piped"));
  let mut io = GatewayIo {
    process,
    stdin,
    stdout,
  };
  let startup = tokio::time::timeout(STARTUP_TIMEOUT, inspect_capability(&mut io.stdout))
    .await
    .unwrap_or_else(|_| {
      Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "shared agent startup timed out",
      ))
    });
  if let Err(error) = startup {
    let missing = agent_is_missing(&error);
    let message = io.process.failure(&error.to_string()).await;
    if missing {
      return Err(io::Error::new(error.kind(), MissingAgent(message)));
    }
    return Err(io::Error::new(error.kind(), message));
  }
  tokio::time::timeout(STARTUP_TIMEOUT, transport::connect(io))
    .await
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::TimedOut,
        "shared agent negotiation timed out",
      )
    })?
}

async fn inspect_capability(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
  let mut consumed = 0;
  loop {
    let line = read_line(reader, MAX_COMPONENT_BYTES).await?;
    consumed += line.len();
    if consumed > MAX_STARTUP_BYTES {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "too much remote output before shared agent startup",
      ));
    }
    if line == MISSING_AGENT_MARKER {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        MissingAgent(
          "ctl-agent is not installed. Install the remote components, then reconnect.".into(),
        ),
      ));
    }
    if line == STARTUP_MARKER {
      break;
    }
  }
  let component = read_line(reader, MAX_COMPONENT_BYTES).await?;
  if component == UNSUPPORTED_AGENT_MARKER {
    return Err(unsupported_agent());
  }
  let component: ctl_core::component::ComponentInfo = serde_json::from_slice(&component)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  if component.protocols.iter().any(|protocol| {
    protocol.name == "ctl_transport"
      && protocol
        .negotiate(transport::SUPPORTED_PROTOCOL_VERSIONS)
        .is_some()
  }) {
    Ok(())
  } else {
    Err(unsupported_agent())
  }
}

async fn read_line(reader: &mut (impl AsyncRead + Unpin), maximum: usize) -> io::Result<Vec<u8>> {
  let mut line = Vec::new();
  while line.len() < maximum {
    let byte = reader.read_u8().await?;
    line.push(byte);
    if byte == b'\n' {
      return Ok(line);
    }
  }
  Err(io::Error::new(
    io::ErrorKind::InvalidData,
    "shared agent startup line is too large",
  ))
}
