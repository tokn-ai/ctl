//! Datagram-preserving forwarding over owned, negotiated agent SSH channels.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ctl_ipc::{LocalPortForward, SshTarget};
use ctl_proto::forwarding;
use tokio::io::AsyncReadExt as _;
use tokio::net::UdpSocket;
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};

use super::ssh_process::Process;
use super::{RequestError, SSH_PROGRAM, append_target_arguments};

const MAX_PEERS: usize = 64;
const PEER_QUEUE: usize = 8;
const PEER_IDLE: Duration = Duration::from_secs(60);
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
    let target_copy = target.clone();
    let forward_copy = forward.clone();
    let path = path.map(Path::to_owned);
    // Verify agent capability and destination before reporting the rule active.
    // Keep this channel for the first peer, avoiding a disposable probe process.
    let initial = Channel::open(command(target, path.as_deref()), forward)
      .await
      .map_err(|error| {
        RequestError::PortForwardFailed(format!("Could not start UDP forwarding: {error}"))
      })?;
    let listener = Self::listener(socket, initial, move || {
      let target = target_copy.clone();
      let forward = forward_copy.clone();
      let path = path.clone();
      async move { Channel::open(command(&target, path.as_deref()), &forward).await }
    });
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

  fn listener<F, Fut>(socket: UdpSocket, initial: Channel, open: F) -> Listener
  where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = io::Result<Channel>> + Send + 'static,
  {
    let (shutdown, mut stop) = oneshot::channel();
    let error = Arc::new(Mutex::new(None));
    let health = Arc::clone(&error);
    let socket = Arc::new(socket);
    let task = tokio::spawn(async move {
      let mut initial = Some(initial);
      let mut peers: HashMap<SocketAddr, mpsc::Sender<Vec<u8>>> = HashMap::new();
      let mut channels = JoinSet::new();
      // Read an extra byte so an oversized datagram cannot be silently truncated.
      let mut buffer = vec![0; forwarding::MAX_DATAGRAM_BYTES + 1];
      let mut observation = tokio::time::interval(Duration::from_secs(1));
      loop {
        tokio::select! {
          biased;
          _ = &mut stop => break,
          result = channels.join_next(), if !channels.is_empty() => {
            if let Some(Ok((peer, result))) = result {
              peers.remove(&peer);
              if let Err(error) = result {
                *health.lock().unwrap() = Some(format!("A UDP forwarding channel failed: {error}. Enable the forward again to reconnect."));
              }
            }
          },
          _ = observation.tick() => {
            if let Some(channel) = &mut initial {
              match channel.process.try_wait() {
                Ok(None) => {},
                result => {
                  *health.lock().unwrap() = Some(format!("The UDP forwarding connection closed ({result:?}). Enable it again to reconnect."));
                  initial = None;
                },
              }
            }
          },
          incoming = socket.recv_from(&mut buffer) => {
            let (size, peer) = match incoming {
              Ok(packet) => packet,
              Err(error) => {
                *health.lock().unwrap() = Some(format!("UDP listener failed: {error}"));
                break;
              },
            };
            if size > forwarding::MAX_DATAGRAM_BYTES { continue; }
            if let Some(sender) = peers.get(&peer) {
              // UDP has no backpressure: discard excess packets instead of
              // blocking every peer or growing an unbounded process queue.
              let _ = sender.try_send(buffer[..size].to_vec());
            } else if peers.len() < MAX_PEERS {
              let (sender, receiver) = mpsc::channel(PEER_QUEUE);
              let _ = sender.try_send(buffer[..size].to_vec());
              peers.insert(peer, sender);
              let socket = Arc::clone(&socket);
              let ready = initial.take();
              let opened = open();
              let health = Arc::clone(&health);
              channels.spawn(async move {
                let result = async {
                  let channel = if let Some(ready) = ready { ready } else { opened.await? };
                  *health.lock().unwrap() = None;
                  channel.relay(socket, peer, receiver).await
                }.await;
                (peer, result)
              });
            }
          },
        }
      }
      channels.shutdown().await;
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
      forwarding::negotiate_contract(&mut stdout, &mut stdin).await?;
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

  async fn relay(
    mut self,
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    mut receiver: mpsc::Receiver<Vec<u8>>,
  ) -> io::Result<()> {
    let (activity, mut observed) = watch::channel(tokio::time::Instant::now());
    let upload = async {
      while let Some(packet) = receiver.recv().await {
        tokio::time::timeout(
          PEER_IDLE,
          forwarding::write_datagram(&mut self.stdin, &packet),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "UDP channel write timed out"))??;
        activity.send_replace(tokio::time::Instant::now());
      }
      Ok(())
    };
    let download = async {
      while let Some(packet) = forwarding::read_datagram(&mut self.stdout).await? {
        socket.send_to(&packet, peer).await?;
        activity.send_replace(tokio::time::Instant::now());
      }
      Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "UDP SSH channel closed",
      ))
    };
    let idle = async {
      loop {
        let deadline = *observed.borrow_and_update() + PEER_IDLE;
        tokio::select! {
          () = tokio::time::sleep_until(deadline) => return Ok(()),
          changed = observed.changed() => {
            if changed.is_err() { return Ok(()); }
          },
        }
      }
    };
    tokio::select! {
      result = upload => result,
      result = download => result,
      result = idle => result,
    }
    // Dropping the process owns cancellation even when an idle peer disappears.
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
