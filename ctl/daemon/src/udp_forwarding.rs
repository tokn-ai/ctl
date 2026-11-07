//! Datagram-preserving forwarding over owned, negotiated agent SSH channels.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use ctl_ipc::{LocalPortForward, SshTarget};
use ctl_proto::forwarding;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite};
use tokio::net::UdpSocket;
use tokio::process::Command;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::ssh_process::Process;
use super::{RequestError, SSH_PROGRAM, append_target_arguments};

const PEER_QUEUE: usize = 8;
const CHANNEL_QUEUE: usize = forwarding::MAX_PEERS * PEER_QUEUE;
const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;
const REMOTE_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-agent is not installed\n' >&2; exit 127; }; "#,
  "exec ctl-agent forward-udp",
);

pub(super) struct Registry {
  listeners: HashMap<(SshTarget, LocalPortForward), Listener>,
  upload_budget: Arc<Semaphore>,
}

impl Default for Registry {
  fn default() -> Self {
    Self {
      listeners: HashMap::new(),
      upload_budget: Arc::new(Semaphore::new(MAX_UPLOAD_BYTES)),
    }
  }
}

struct Listener {
  shutdown: Option<oneshot::Sender<()>>,
  task: Option<JoinHandle<()>>,
  error: Arc<Mutex<Option<String>>>,
}

pub(super) struct Prepared {
  key: (SshTarget, LocalPortForward),
  listener: Listener,
}

impl Prepared {
  pub(super) async fn stop(self) {
    self.listener.stop().await;
  }
}

impl Drop for Listener {
  fn drop(&mut self) {
    if let Some(task) = &self.task {
      task.abort();
    }
  }
}

impl Listener {
  async fn stop(mut self) {
    if let Some(shutdown) = self.shutdown.take() {
      let _ = shutdown.send(());
    }
    if let Some(task) = self.task.take() {
      let _ = task.await;
    }
  }
}

impl Registry {
  pub(super) async fn prepare(
    &mut self,
    target: &SshTarget,
    path: Option<&Path>,
    forward: &LocalPortForward,
    gateways: &super::agent_transport::Registry,
  ) -> Result<Prepared, RequestError> {
    super::validate_forward(forward)?;
    self.cancel(target, forward).await;
    // Reserve UDP before opening SSH, independently of TCP on the same port.
    let socket = UdpSocket::bind((forward.bind_address.as_str(), forward.local_port))
      .await
      .map_err(|error| super::port_forwarding::local_bind_error(forward, &error))?;
    // Verify agent capability and destination before reporting the rule active.
    // Rules and terminal views use logical streams on the same agent command.
    // Native Windows SSH has no shared master and retains its direct channel.
    let channel = match path {
      Some(path) => {
        let stream = gateways
          .open(target, path, ctl_proto::transport::Service::Udp)
          .await
          .map_err(RequestError::AgentTransport)?;
        Channel::shared(stream, forward).await
      }
      None => Channel::open(command(target, None), forward).await,
    }
    .map_err(|error| {
      RequestError::PortForwardFailed(format!("Could not start UDP forwarding: {error}"))
    })?;
    let listener = self.listener(socket, channel);
    Ok(Prepared {
      key: (target.clone(), forward.clone()),
      listener,
    })
  }

  pub(super) fn commit(&mut self, prepared: Prepared) {
    self.listeners.insert(prepared.key, prepared.listener);
  }

  pub(super) async fn cancel_target(&mut self, target: &SshTarget) {
    let keys: Vec<_> = self
      .listeners
      .keys()
      .filter(|(owner, _)| owner == target)
      .cloned()
      .collect();
    for key in keys {
      if let Some(listener) = self.listeners.remove(&key) {
        listener.stop().await;
      }
    }
  }

  pub(super) async fn shutdown(&mut self) {
    for (_, listener) in self.listeners.drain() {
      listener.stop().await;
    }
  }

  pub(super) fn check(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
  ) -> Result<(), RequestError> {
    let listener = self
      .listeners
      .get(&(target.clone(), forward.clone()))
      .ok_or_else(|| {
        RequestError::PortForwardFailed(
          "The UDP forwarding listener stopped. Enable it again to reconnect.".into(),
        )
      })?;
    if let Some(error) = &*listener.error.lock().unwrap() {
      return Err(RequestError::PortForwardFailed(error.clone()));
    }
    if listener.task.as_ref().is_none_or(JoinHandle::is_finished) {
      return Err(RequestError::PortForwardFailed(
        "The UDP forwarding listener stopped. Enable it again to reconnect.".into(),
      ));
    }
    Ok(())
  }

  pub(super) async fn cancel(&mut self, target: &SshTarget, forward: &LocalPortForward) {
    if let Some(listener) = self.listeners.remove(&(target.clone(), forward.clone())) {
      listener.stop().await;
    }
  }

  fn listener(&self, socket: UdpSocket, channel: Channel) -> Listener {
    let (shutdown, stop) = oneshot::channel();
    let error = Arc::new(Mutex::new(None));
    let health = Arc::clone(&error);
    let upload_budget = Arc::clone(&self.upload_budget);
    let task = tokio::spawn(async move {
      let Channel {
        mut process,
        mut stdin,
        mut stdout,
      } = channel;
      let result = relay_socket(&socket, &mut stdin, &mut stdout, stop, upload_budget).await;
      drop(socket);
      if let Err(error) = result {
        let message = match &mut process {
          Some(process) => process.failure(&error.to_string()).await,
          None => error.to_string(),
        };
        *health.lock().unwrap() = Some(format!(
          "The UDP forwarding channel failed: {message}. Enable the forward again to reconnect."
        ));
      }
    });
    Listener {
      shutdown: Some(shutdown),
      task: Some(task),
      error,
    }
  }
}

struct Channel {
  process: Option<Process>,
  stdin: Box<dyn AsyncWrite + Unpin + Send>,
  stdout: Box<dyn AsyncRead + Unpin + Send>,
}

impl Channel {
  async fn open(command: Command, forward: &LocalPortForward) -> io::Result<Self> {
    let mut process = Process::spawn(command)?;
    let stdin = Box::new(process.stdin().expect("UDP SSH stdin is piped"));
    let stdout = Box::new(process.stdout().expect("UDP SSH stdout is piped"));
    Self::ready(
      Self {
        process: Some(process),
        stdin,
        stdout,
      },
      forward,
    )
    .await
  }

  async fn shared(
    stream: ctl_proto::transport::Stream,
    forward: &LocalPortForward,
  ) -> io::Result<Self> {
    let (reader, writer) = tokio::io::split(stream);
    Self::ready(
      Self {
        process: None,
        stdin: Box::new(writer),
        stdout: Box::new(reader),
      },
      forward,
    )
    .await
  }

  async fn ready(mut self, forward: &LocalPortForward) -> io::Result<Self> {
    let startup = tokio::time::timeout(forwarding::STARTUP_TIMEOUT, async {
      let mut marker = vec![0; forwarding::PREFACE.len()];
      self.stdout.read_exact(&mut marker).await?;
      if marker != forwarding::PREFACE {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Unsupported remote UDP forwarding endpoint",
        ));
      }
      forwarding::negotiate_multiplex_contract(&mut self.stdout, &mut self.stdin).await?;
      forwarding::write(
        &mut self.stdin,
        &forwarding::Request {
          host: forward.remote_host.clone(),
          port: forward.remote_port,
        },
      )
      .await?;
      match forwarding::read::<forwarding::Response>(&mut self.stdout).await? {
        forwarding::Response::Ready => Ok(()),
        forwarding::Response::Error { message } => Err(io::Error::other(message)),
      }
    })
    .await
    .unwrap_or_else(|_| {
      Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "UDP forwarding startup timed out",
      ))
    });
    if let Err(error) = startup {
      let message = match &mut self.process {
        Some(process) => process.failure(&error.to_string()).await,
        None => error.to_string(),
      };
      return Err(io::Error::other(message));
    }
    Ok(self)
  }
}

struct Peer {
  id: u64,
  last_activity: Instant,
}

#[derive(Debug)]
struct Upload {
  packet: forwarding::Frame,
  // Keep bytes reserved until the payload is written or discarded. Fields
  // drop in declaration order, so payload memory is freed before its budget.
  _payload_bytes: Option<OwnedSemaphorePermit>,
}

impl Upload {
  fn close(peer_id: u64) -> Self {
    Self {
      packet: forwarding::Frame::Close { peer_id },
      _payload_bytes: None,
    }
  }
}

fn try_queue_datagram(
  outgoing: &mpsc::Sender<Upload>,
  budget: &Arc<Semaphore>,
  peer_id: u64,
  payload: &[u8],
) -> bool {
  // Reserve both limits before copying data. A full channel or a busy sibling
  // rule drops this datagram without allocating or waiting for SSH to drain.
  let Ok(slot) = outgoing.try_reserve() else {
    return false;
  };
  let Ok(bytes) = u32::try_from(payload.len()) else {
    return false;
  };
  let Ok(permit) = Arc::clone(budget).try_acquire_many_owned(bytes) else {
    return false;
  };
  slot.send(Upload {
    packet: forwarding::Frame::Datagram {
      peer_id,
      payload: payload.to_vec(),
    },
    _payload_bytes: Some(permit),
  });
  true
}

async fn write_uploads(
  stdin: &mut (impl AsyncWrite + Unpin),
  mut uploads: mpsc::Receiver<Upload>,
) -> io::Result<()> {
  while let Some(upload) = uploads.recv().await {
    tokio::time::timeout(
      forwarding::PEER_IDLE,
      forwarding::write_packet(stdin, &upload.packet),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP channel write timed out"))??;
  }
  Ok(())
}

/// One pair of complete framed I/O loops serves every local UDP association.
/// Never cancel an individual frame read when another peer or timer is ready.
async fn relay_socket(
  socket: &UdpSocket,
  stdin: &mut (impl AsyncWrite + Unpin),
  stdout: &mut (impl AsyncRead + Unpin),
  mut stop: oneshot::Receiver<()>,
  upload_budget: Arc<Semaphore>,
) -> io::Result<()> {
  let (outgoing, uploads) = mpsc::channel(CHANNEL_QUEUE);
  let (downloads, mut incoming) = mpsc::channel(PEER_QUEUE);
  let upload = write_uploads(stdin, uploads);
  let download = async {
    while let Some(packet) = forwarding::read_packet(stdout).await? {
      if downloads.send(packet).await.is_err() {
        return Ok(());
      }
    }
    Err(io::Error::new(
      io::ErrorKind::UnexpectedEof,
      "UDP SSH channel closed",
    ))
  };
  tokio::pin!(upload, download);
  let mut peers: HashMap<SocketAddr, Peer> = HashMap::new();
  let mut addresses: HashMap<u64, SocketAddr> = HashMap::new();
  let mut next_id = 1_u64;
  // Accept the full UDP receive range, then discard payloads over our framing
  // bound. A valid large IPv6 datagram must not terminate a Windows listener.
  let mut buffer = vec![0; 65_536];
  let mut expiration = tokio::time::interval(std::time::Duration::from_secs(1));
  loop {
    tokio::select! {
      _ = &mut stop => return Ok(()),
      result = &mut upload => return result,
      result = &mut download => return result,
      _ = expiration.tick() => {
        let now = Instant::now();
        peers.retain(|_, peer| {
          if now.duration_since(peer.last_activity) < forwarding::PEER_IDLE { return true; }
          // Keep an expired association reserved if the bounded channel is
          // busy. Retrying the close avoids orphaning a remote peer socket.
          if outgoing.try_send(Upload::close(peer.id)).is_err() { return true; }
          addresses.remove(&peer.id);
          false
        });
      },
      packet = incoming.recv() => {
        match packet {
          Some(forwarding::Frame::Datagram { peer_id, payload }) => {
            if let Some(address) = addresses.get(&peer_id)
              && socket.send_to(&payload, address).await.is_ok()
              && let Some(peer) = peers.get_mut(address)
            {
              peer.last_activity = Instant::now();
            }
          },
          Some(forwarding::Frame::Close { peer_id }) => {
            if let Some(address) = addresses.remove(&peer_id) { peers.remove(&address); }
          },
          None => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "UDP SSH channel closed")),
        }
      },
      received = socket.recv_from(&mut buffer) => {
        let (size, address) = match received {
          Ok(packet) => packet,
          Err(error) if matches!(error.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused) => continue,
          Err(error) => return Err(error),
        };
        if size > forwarding::MAX_DATAGRAM_BYTES { continue; }
        let id = match peers.get(&address) {
          Some(peer) => peer.id,
          None if peers.len() < forwarding::MAX_PEERS => next_id,
          None => continue,
        };
        // UDP has no backpressure: drop excess data rather than accumulating
        // unbounded memory or blocking every other client on this listener.
        if !try_queue_datagram(&outgoing, &upload_budget, id, &buffer[..size]) { continue; }
        if let Some(peer) = peers.get_mut(&address) {
          peer.last_activity = Instant::now();
        } else {
          next_id = next_id.checked_add(1).ok_or_else(|| io::Error::other("UDP peer IDs exhausted"))?;
          peers.insert(address, Peer { id, last_activity: Instant::now() });
          addresses.insert(id, address);
        }
      },
    }
  }
}

fn command(target: &SshTarget, path: Option<&Path>) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command.args(["-T", "-o", "ControlMaster=no"]);
  if let Some(path) = path {
    command
      .arg("-S")
      .arg(path)
      .args(["-o", "ProxyCommand=false"]);
  } else {
    command.args(["-o", "ControlPath=none", "-o", "ControlPersist=no"]);
  }
  command
    .args(["-o", "BatchMode=yes", "-o", "StdinNull=no"])
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
