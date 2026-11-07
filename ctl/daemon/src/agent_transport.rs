//! One authenticated agent command carries every logical service on a master.

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
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
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
  gateway: Option<Gateway>,
}

#[derive(Default)]
pub(super) struct Registry {
  gateways: Mutex<HashMap<PathBuf, Arc<GatewaySlot>>>,
}

struct Gateway {
  socket_identity: (u64, u64),
  client: Client,
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
    let _target_guard = attempt
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
    let remote = attempt
      .run(state.agent_transports.open(target, control_path, service))
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
      .open_with(target, control_path, service, || {
        start(command(target, control_path))
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
    F: FnOnce() -> Fut,
    Fut: Future<Output = io::Result<Client>>,
  {
    // Aliases can resolve to the same master. Canonicalize symlinks as well so
    // the authenticated control socket, rather than a target spelling, owns it.
    let control_path = std::fs::canonicalize(control_path)?;
    let (slot, mut cancelled) = {
      let mut gateways = self.gateways.lock().unwrap();
      let slot = gateways.entry(control_path.clone()).or_default();
      let cancelled = slot
        .owned
        .lock()
        .unwrap()
        .targets
        .entry(target.clone())
        .or_insert_with(|| watch::channel(false).0)
        .subscribe();
      (Arc::clone(slot), cancelled)
    };
    let availability = cancelled.clone();
    let open = async {
      let client = {
        // Ownership uses a separate short lock. Disconnect never waits for
        // remote startup, including another alias initializing this socket.
        let _initializing = slot.initialization.lock().await;
        let socket_identity = socket_identity(&control_path)?;
        let existing = {
          let mut owned = slot.owned.lock().unwrap();
          if owned.gateway.as_ref().is_some_and(|entry| {
            entry.socket_identity != socket_identity || entry.client.is_closed()
          }) && let Some(previous) = owned.gateway.take()
          {
            previous.client.close();
            // Previous aliases owned the replaced physical connection. Their
            // pending opens must retry against its new identity, and must not
            // keep a replacement gateway alive after its new owner leaves.
            owned.targets.retain(|owner, cancelled| {
              if owner == target {
                true
              } else {
                cancelled.send_replace(true);
                false
              }
            });
          }
          owned.gateway.as_ref().map(|entry| entry.client.clone())
        };
        if let Some(client) = existing {
          client
        } else {
          let client = connect().await?;
          let mut owned = slot.owned.lock().unwrap();
          if *availability.borrow() {
            // A cancelled initializer may finish in the same poll as its
            // disconnect. Never publish a client into a detached old slot.
            client.close();
            return Err(cancelled_open());
          }
          owned.gateway = Some(Gateway {
            socket_identity,
            client: client.clone(),
          });
          client
        }
      };
      client.open(service).await
    };
    tokio::select! {
      biased;
      _ = cancelled.wait_for(|cancelled| *cancelled) => Err(cancelled_open()),
      result = open => result,
    }
  }

  pub(super) fn cancel_target(&self, target: &SshTarget) {
    self.gateways.lock().unwrap().retain(|_, slot| {
      let mut owned = slot.owned.lock().unwrap();
      if let Some(cancelled) = owned.targets.remove(target) {
        cancelled.send_replace(true);
      }
      if !owned.targets.is_empty() {
        return true;
      }
      if let Some(gateway) = owned.gateway.take() {
        gateway.client.close();
      }
      false
    });
  }

  pub(super) fn shutdown(&self) {
    let slots: Vec<_> = self
      .gateways
      .lock()
      .unwrap()
      .drain()
      .map(|(_, slot)| slot)
      .collect();
    for slot in slots {
      let mut owned = slot.owned.lock().unwrap();
      for (_, cancelled) in owned.targets.drain() {
        cancelled.send_replace(true);
      }
      if let Some(gateway) = owned.gateway.take() {
        gateway.client.close();
      }
    }
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
