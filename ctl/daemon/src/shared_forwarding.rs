//! Local forwarding listeners owned by ctld, independent of a shared SSH master.

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::path::Path;
use std::process::Stdio;

use ctld_ipc::{LocalPortForward, SshTarget};
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

use super::{
  RequestError, SSH_PROGRAM, append_target_arguments, remote_forward_host, validate_forward,
};

#[derive(Default)]
pub(super) struct SharedForwardRegistry {
  listeners: HashMap<(SshTarget, LocalPortForward), OwnedListener>,
}

struct OwnedListener {
  shutdown: Option<oneshot::Sender<()>>,
  task: Option<JoinHandle<()>>,
}

impl Drop for OwnedListener {
  fn drop(&mut self) {
    if let Some(task) = &self.task {
      task.abort();
    }
  }
}

impl OwnedListener {
  async fn stop(mut self) {
    if let Some(shutdown) = self.shutdown.take() {
      let _ = shutdown.send(());
    }
    if let Some(task) = self.task.take() {
      let _ = task.await;
    }
  }
}

impl SharedForwardRegistry {
  pub(super) async fn start(
    &mut self,
    target: &SshTarget,
    control_path: &Path,
    forward: &LocalPortForward,
  ) -> Result<(), RequestError> {
    let channel_target = target.clone();
    let channel_path = control_path.to_owned();
    let channel_forward = forward.clone();
    self
      .start_with(target, forward, move |stream| {
        let target = channel_target.clone();
        let path = channel_path.clone();
        let forward = channel_forward.clone();
        async move {
          let _ = relay_ssh_channel(stream, &target, &path, &forward).await;
        }
      })
      .await
  }

  async fn start_with<F, Fut>(
    &mut self,
    target: &SshTarget,
    forward: &LocalPortForward,
    open_channel: F,
  ) -> Result<(), RequestError>
  where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    validate_forward(forward)?;
    let key = (target.clone(), forward.clone());
    if self.listeners.contains_key(&key) {
      return Ok(());
    }
    // Binding the exact address also detects a collision with a listener owned
    // by Terminal or another application. Never cancel the shared master's
    // forwarding configuration to make this port available.
    let listener = TcpListener::bind((forward.bind_address.as_str(), forward.local_port))
      .await
      .map_err(|error| RequestError::PortForwardFailed(error.to_string()))?;
    self.start_listener(key, listener, open_channel);
    Ok(())
  }

  fn start_listener<F, Fut>(
    &mut self,
    key: (SshTarget, LocalPortForward),
    listener: TcpListener,
    open_channel: F,
  ) where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let (shutdown, mut stop) = oneshot::channel();
    let task = tokio::spawn(async move {
      let mut channels = JoinSet::new();
      loop {
        tokio::select! {
          biased;
          _ = &mut stop => break,
          accepted = listener.accept() => match accepted {
            Ok((stream, _)) => { channels.spawn(open_channel(stream)); }
            Err(_) => break,
          },
          _ = channels.join_next(), if !channels.is_empty() => {},
        }
      }
      drop(listener);
      // Aborting channel futures drops their SSH children (kill_on_drop) and
      // TCP sockets. Wait for that cleanup before reporting disconnection.
      channels.shutdown().await;
    });
    self.listeners.insert(
      key,
      OwnedListener {
        shutdown: Some(shutdown),
        task: Some(task),
      },
    );
  }

  /// Returns whether this registry owned the exact forwarding definition.
  pub(super) async fn cancel(&mut self, target: &SshTarget, forward: &LocalPortForward) -> bool {
    let Some(listener) = self.listeners.remove(&(target.clone(), forward.clone())) else {
      return false;
    };
    listener.stop().await;
    true
  }
}

fn channel_command(target: &SshTarget, path: &Path, forward: &LocalPortForward) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command
    .arg("-S")
    .arg(path)
    .args(["-T", "-o", "ControlMaster=no"])
    .args(["-o", "ProxyCommand=false"])
    .args(["-o", "BatchMode=yes"])
    .args(["-o", "StdinNull=no"])
    .args(["-o", "ForkAfterAuthentication=no"])
    .args(["-o", "ClearAllForwardings=yes"])
    .args(["-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no"])
    .args(["-o", "PermitLocalCommand=no"])
    .args(["-o", "RemoteCommand=none"])
    .arg("-W")
    .arg(format!(
      "{}:{}",
      remote_forward_host(&forward.remote_host),
      forward.remote_port
    ));
  append_target_arguments(&mut command, target);
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  command
}

async fn relay_ssh_channel(
  stream: TcpStream,
  target: &SshTarget,
  path: &Path,
  forward: &LocalPortForward,
) -> io::Result<()> {
  relay_channel(stream, channel_command(target, path, forward)).await
}

async fn relay_channel(stream: TcpStream, mut command: Command) -> io::Result<()> {
  let mut child = command.spawn()?;
  let mut stdin = child.stdin.take().expect("SSH stdin is piped");
  let mut stdout = child.stdout.take().expect("SSH stdout is piped");
  let (mut reader, mut writer) = stream.into_split();
  let upload = async move {
    tokio::io::copy(&mut reader, &mut stdin).await?;
    // Closing the pipe forwards a local TCP half-close as SSH channel EOF.
    // Keeping ChildStdin alive while awaiting stdout would deadlock peers
    // that produce their final response only after receiving EOF.
    drop(stdin);
    Ok::<_, io::Error>(())
  };
  let download = async {
    tokio::io::copy(&mut stdout, &mut writer).await?;
    writer.shutdown().await
  };
  tokio::try_join!(upload, download)?;
  child.wait().await?;
  Ok(())
}
