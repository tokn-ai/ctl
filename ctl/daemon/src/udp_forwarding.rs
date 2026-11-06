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
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::ssh_process::Process;
use super::{RequestError, SSH_PROGRAM, append_target_arguments};

const PEER_QUEUE: usize = 8;
const CHANNEL_QUEUE: usize = forwarding::MAX_PEERS * PEER_QUEUE;
const REMOTE_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-agent is not installed\n' >&2; exit 127; }; "#,
  "exec ctl-agent forward-udp",
);

#[derive(Default)]
pub(super) struct Registry {
  listeners: HashMap<(SshTarget, LocalPortForward), Listener>,
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
  ) -> Result<Prepared, RequestError> {
    super::validate_forward(forward)?;
    self.cancel(target, forward).await;
    // Reserve UDP before opening SSH, independently of TCP on the same port.
    let socket = UdpSocket::bind((forward.bind_address.as_str(), forward.local_port))
      .await
      .map_err(|error| super::port_forwarding::local_bind_error(forward, &error))?;
    // Verify agent capability and destination before reporting the rule active.
    // Every peer uses this single SSH session, leaving the master's remaining
    // command sessions available to terminals and other ctl operations.
    let channel = Channel::open(command(target, path), forward)
      .await
      .map_err(|error| {
        RequestError::PortForwardFailed(format!("Could not start UDP forwarding: {error}"))
      })?;
    let listener = Self::listener(socket, channel);
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

  fn listener(socket: UdpSocket, channel: Channel) -> Listener {
    let (shutdown, stop) = oneshot::channel();
    let error = Arc::new(Mutex::new(None));
    let health = Arc::clone(&error);
    let task = tokio::spawn(async move {
      let Channel {
        mut process,
        mut stdin,
        mut stdout,
      } = channel;
      let result = relay_socket(&socket, &mut stdin, &mut stdout, stop).await;
      drop(socket);
      if let Err(error) = result {
        let message = process.failure(&error.to_string()).await;
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
  process: Process,
  stdin: ChildStdin,
  stdout: ChildStdout,
}

impl Channel {
  async fn open(command: Command, forward: &LocalPortForward) -> io::Result<Self> {
    let mut process = Process::spawn(command)?;
    let mut stdin = process.stdin().expect("UDP SSH stdin is piped");
    let mut stdout = process.stdout().expect("UDP SSH stdout is piped");
    let startup = tokio::time::timeout(forwarding::STARTUP_TIMEOUT, async {
      let mut marker = vec![0; forwarding::PREFACE.len()];
      stdout.read_exact(&mut marker).await?;
      if marker != forwarding::PREFACE {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Unsupported remote UDP forwarding endpoint",
        ));
      }
      forwarding::negotiate_multiplex_contract(&mut stdout, &mut stdin).await?;
      forwarding::write(
        &mut stdin,
        &forwarding::Request {
          host: forward.remote_host.clone(),
          port: forward.remote_port,
        },
      )
      .await?;
      match forwarding::read::<forwarding::Response>(&mut stdout).await? {
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
      return Err(io::Error::other(process.failure(&error.to_string()).await));
    }
    Ok(Self {
      process,
      stdin,
      stdout,
    })
  }
}

struct Peer {
  id: u64,
  last_activity: Instant,
}

/// One pair of complete framed I/O loops serves every local UDP association.
/// Never cancel an individual frame read when another peer or timer is ready.
async fn relay_socket(
  socket: &UdpSocket,
  stdin: &mut (impl AsyncWrite + Unpin),
  stdout: &mut (impl AsyncRead + Unpin),
  mut stop: oneshot::Receiver<()>,
) -> io::Result<()> {
  let (outgoing, mut uploads) = mpsc::channel(CHANNEL_QUEUE);
  let (downloads, mut incoming) = mpsc::channel(PEER_QUEUE);
  let upload = async {
    while let Some(packet) = uploads.recv().await {
      tokio::time::timeout(
        forwarding::PEER_IDLE,
        forwarding::write_packet(stdin, &packet),
      )
      .await
      .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP channel write timed out"))??;
    }
    Ok(())
  };
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
          if outgoing.try_send(forwarding::Frame::Close { peer_id: peer.id }).is_err() { return true; }
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
        let packet = forwarding::Frame::Datagram { peer_id: id, payload: buffer[..size].to_vec() };
        // UDP has no backpressure: drop excess data rather than accumulating
        // unbounded memory or blocking every other client on this listener.
        if outgoing.try_send(packet).is_err() { continue; }
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
