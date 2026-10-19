//! Windows forwards use ordinary SSH connections, independently of `ControlMaster`.
//!
//! A live authenticated anchor supervises each listener. Every accepted TCP
//! connection uses a fresh `ssh -W` transport, preserving OpenSSH's own config
//! evaluation while `ClearAllForwardings` suppresses unrelated config forwards.

mod process;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::io;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ctl_ipc::{LocalPortForward, SshTarget};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use super::shared_forwarding::{OwnedListener, relay_stream};
use super::{RequestError, SSH_PROGRAM, append_target_arguments, remote_forward_host};
use process::Process;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_READY_BYTES: usize = 256;

#[derive(Default)]
pub(super) struct Registry {
  listeners: HashMap<(SshTarget, LocalPortForward), Listener>,
}

struct Listener {
  owner: OwnedListener,
  health: Arc<Mutex<Health>>,
}

#[derive(Default)]
struct Health {
  anchor_error: Option<String>,
  channel_error: Option<String>,
}

enum ChannelOutcome {
  Completed,
  ClientClosed,
}

impl Registry {
  /// Passive observation never starts SSH or retries authentication.
  pub(super) fn check(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
  ) -> Result<(), RequestError> {
    if target.use_ssh_config_master == Some(true) {
      return Err(RequestError::UnsupportedPlatform("SSH ControlMaster reuse"));
    }
    let Some(listener) = self.listeners.get(&(target.clone(), forward.clone())) else {
      return Err(RequestError::PortForwardFailed(
        "The native SSH forwarding listener is no longer running. Enable it again to reconnect."
          .into(),
      ));
    };
    let health = listener.health.lock().unwrap();
    if let Some(error) = health
      .anchor_error
      .as_ref()
      .or(health.channel_error.as_ref())
    {
      return Err(RequestError::PortForwardFailed(error.clone()));
    }
    if listener.owner.is_finished() {
      return Err(RequestError::PortForwardFailed(
        "The native SSH forwarding listener stopped. Enable it again to reconnect.".into(),
      ));
    }
    Ok(())
  }

  pub(super) async fn start(
    &mut self,
    target: &SshTarget,
    forward: &LocalPortForward,
  ) -> Result<(), RequestError> {
    let nonce = readiness_nonce();
    self
      .start_with(target, forward, anchor_command(target, &nonce), nonce)
      .await
  }

  async fn start_with(
    &mut self,
    target: &SshTarget,
    forward: &LocalPortForward,
    command: Command,
    nonce: String,
  ) -> Result<(), RequestError> {
    if target.use_ssh_config_master == Some(true) {
      return Err(RequestError::UnsupportedPlatform("SSH ControlMaster reuse"));
    }
    super::validate_forward(forward)?;
    if self.check(target, forward).is_ok() {
      return Ok(());
    }
    self.cancel(target, forward).await;
    // Reserve the exact endpoint before authentication. An occupied port must
    // not authenticate, adopt, or disturb another application's listener.
    let listener = TcpListener::bind((forward.bind_address.as_str(), forward.local_port))
      .await
      .map_err(|error| super::port_forwarding::local_bind_error(forward, &error))?;
    self
      .start_listener(target, forward, command, nonce, listener)
      .await
  }

  async fn start_listener(
    &mut self,
    target: &SshTarget,
    forward: &LocalPortForward,
    command: Command,
    nonce: String,
    listener: TcpListener,
  ) -> Result<(), RequestError> {
    let mut anchor = Process::spawn(command).map_err(|error| {
      RequestError::PortForwardFailed(format!("Could not start native SSH forwarding: {error}"))
    })?;
    let mut stdout = BufReader::new(anchor.stdout().expect("SSH anchor stdout is piped"));
    if let Err(error) = wait_anchor_ready(&mut anchor, &mut stdout, &nonce).await {
      return Err(RequestError::PortForwardFailed(
        anchor.failure(&error.to_string()).await,
      ));
    }
    drop(stdout);

    let health = Arc::new(Mutex::new(Health::default()));
    let anchor_health = Arc::clone(&health);
    let supervisor = async move {
      let reason = match anchor.wait().await {
        Ok(status) => format!("The SSH forwarding connection closed ({status})"),
        Err(error) => format!("Could not observe the SSH forwarding connection: {error}"),
      };
      let message = anchor.failure(&reason).await;
      anchor_health.lock().unwrap().anchor_error = Some(message);
    };
    let channel_target = target.clone();
    let channel_forward = forward.clone();
    let channel_health = Arc::clone(&health);
    let owner = OwnedListener::spawn(
      listener,
      move |stream| {
        let target = channel_target.clone();
        let forward = channel_forward.clone();
        let health = Arc::clone(&channel_health);
        async move {
          match relay(stream, &target, &forward).await {
            Ok(ChannelOutcome::Completed) => health.lock().unwrap().channel_error = None,
            Ok(ChannelOutcome::ClientClosed) => {}
            Err(error) => {
              health.lock().unwrap().channel_error = Some(format!(
                "An SSH forwarding channel failed: {error}. Enable the forward again to reconnect."
              ));
            }
          }
        }
      },
      supervisor,
    );
    self.listeners.insert(
      (target.clone(), forward.clone()),
      Listener { owner, health },
    );
    Ok(())
  }

  pub(super) async fn cancel(&mut self, target: &SshTarget, forward: &LocalPortForward) {
    if let Some(listener) = self.listeners.remove(&(target.clone(), forward.clone())) {
      listener.owner.stop().await;
    }
  }
}

fn readiness_nonce() -> String {
  format!("ctld-forward-ready-{}", uuid::Uuid::new_v4().simple())
}

fn connection_command(nonce: &str) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command
    .arg("-T")
    .args(["-o", "ControlMaster=no"])
    .args(["-o", "ControlPath=none"])
    .args(["-o", "ControlPersist=no"])
    .args(["-o", "ForkAfterAuthentication=no"])
    .args(["-o", "RemoteCommand=none"])
    .args(["-o", "ClearAllForwardings=yes"])
    .args(["-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no"])
    .args(["-o", "Tunnel=no"])
    .args(["-o", "BatchMode=yes"])
    .args(["-o", "ConnectTimeout=10"])
    .args(["-o", "ConnectionAttempts=1"])
    .args(["-o", "ServerAliveInterval=10"])
    .args(["-o", "ServerAliveCountMax=3"])
    .args(["-o", "ExitOnForwardFailure=yes"])
    .args(["-o", "PermitLocalCommand=yes"])
    // OpenSSH executes this after authentication, before transferring stdout
    // to the forwarding channel. Only our generated ASCII nonce enters the
    // local shell; config LocalCommand and RemoteCommand are overridden.
    .arg("-o")
    .arg(format!("LocalCommand=echo {nonce}"))
    .env("SSH_ASKPASS_REQUIRE", "never")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  command
}

fn anchor_command(target: &SshTarget, nonce: &str) -> Command {
  let mut command = connection_command(nonce);
  command
    .arg("-N")
    .args(["-o", "StdinNull=yes"])
    .stdin(Stdio::null());
  append_target_arguments(&mut command, target);
  command
}

fn channel_command(target: &SshTarget, forward: &LocalPortForward, nonce: &str) -> Command {
  let mut command = connection_command(nonce);
  command.args(["-o", "StdinNull=no"]).arg("-W").arg(format!(
    "{}:{}",
    remote_forward_host(&forward.remote_host),
    forward.remote_port
  ));
  append_target_arguments(&mut command, target);
  command
}

async fn wait_anchor_ready(
  process: &mut Process,
  stdout: &mut (impl AsyncBufRead + Unpin),
  nonce: &str,
) -> io::Result<()> {
  tokio::time::timeout(START_TIMEOUT, async {
    tokio::select! {
      biased;
      status = process.wait() => {
        Err(io::Error::other(format!("SSH exited before forwarding authentication completed ({})", status?)))
      },
      result = read_nonce(stdout, nonce) => {
        result?;
        if let Some(status) = process.try_wait()? {
          return Err(io::Error::other(format!("SSH exited during forwarding startup ({status})")));
        }
        Ok(())
      },
    }
  })
  .await
  .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "SSH forwarding authentication timed out"))?
}

async fn read_nonce(stdout: &mut (impl AsyncBufRead + Unpin), nonce: &str) -> io::Result<()> {
  let mut line = Vec::new();
  loop {
    let available = stdout.fill_buf().await?;
    if available.is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "SSH closed before confirming forwarding authentication",
      ));
    }
    let count = available
      .iter()
      .position(|byte| *byte == b'\n')
      .map_or(available.len(), |index| index + 1);
    if line.len() + count > MAX_READY_BYTES {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid SSH forwarding readiness response",
      ));
    }
    line.extend_from_slice(&available[..count]);
    stdout.consume(count);
    if line.last() == Some(&b'\n') {
      line.pop();
      if line.last() == Some(&b'\r') {
        line.pop();
      }
      return if line == nonce.as_bytes() {
        Ok(())
      } else {
        Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Invalid SSH forwarding readiness response",
        ))
      };
    }
  }
}

async fn relay(
  stream: TcpStream,
  target: &SshTarget,
  forward: &LocalPortForward,
) -> io::Result<ChannelOutcome> {
  let nonce = readiness_nonce();
  let process = Process::spawn(channel_command(target, forward, &nonce))?;
  relay_process(stream, process, &nonce).await
}

async fn relay_process(
  stream: TcpStream,
  mut process: Process,
  nonce: &str,
) -> io::Result<ChannelOutcome> {
  let mut stdout = BufReader::new(process.stdout().expect("SSH channel stdout is piped"));
  // A short-lived channel can exit with a complete response still buffered
  // after its nonce. Drain the response before checking its exit status.
  let readiness = tokio::time::timeout(START_TIMEOUT, read_nonce(&mut stdout, nonce))
    .await
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::TimedOut,
        "SSH forwarding authentication timed out",
      )
    })
    .and_then(std::convert::identity);
  if let Err(error) = readiness {
    return Err(io::Error::other(process.failure(&error.to_string()).await));
  }
  let stdin = process.stdin().expect("SSH channel stdin is piped");
  if let Err(error) = relay_stream(stream, stdin, stdout).await {
    // After authenticated startup, stream I/O combines the local TCP peer and
    // SSH pipes. These disconnect kinds commonly mean a browser cancelled a
    // request; they do not establish failure of the forwarding configuration.
    // Pre-readiness/authentication errors and nonzero SSH exits remain errors.
    if matches!(
      error.kind(),
      io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::NotConnected
    ) {
      return Ok(ChannelOutcome::ClientClosed);
    }
    return Err(error);
  }
  let status = process.wait().await?;
  if status.success() {
    Ok(ChannelOutcome::Completed)
  } else {
    Err(io::Error::other(
      process
        .failure(&format!("SSH forwarding channel exited ({status})"))
        .await,
    ))
  }
}
