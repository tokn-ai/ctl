//! Local and OpenSSH transport primitives for `ctl`.
//!
//! Local connections use the owner-only `rmuxd` Unix endpoint. Remote
//! authentication, host verification, proxying, and connection multiplexing
//! belong to the user's OpenSSH installation and configuration.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

const SSH_PROGRAM: &str = "ssh";
const REMOTE_COMMAND: [&str; 3] = ["exec", "ctld", "connect"];
const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

/// The daemon endpoint selected for one `ctl` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
  /// The current user's owner-only local `rmuxd` endpoint.
  Local { socket_path: PathBuf },
  /// An OpenSSH destination or `Host` alias.
  Ssh { destination: String },
}

impl ConnectionTarget {
  /// Selects the current user's default local `rmuxd` endpoint.
  #[must_use]
  pub fn local() -> Self {
    Self::Local {
      socket_path: rmux_ipc::socket_path(),
    }
  }

  /// Selects an OpenSSH destination or `Host` alias.
  #[must_use]
  pub fn ssh(destination: impl Into<String>) -> Self {
    Self::Ssh {
      destination: destination.into(),
    }
  }

  /// Returns a concise name suitable for user-facing status messages.
  #[must_use]
  pub fn label(&self) -> &str {
    match self {
      Self::Local { .. } => "local",
      Self::Ssh { destination } => destination,
    }
  }

  /// Returns whether this target uses the local owner-only endpoint.
  #[must_use]
  pub fn is_local(&self) -> bool {
    matches!(self, Self::Local { .. })
  }
}

/// A raw `rmux-proto` stream over either the local socket or OpenSSH.
pub enum Transport {
  #[cfg(unix)]
  Local(UnixStream),
  Ssh(SshTransport),
}

impl AsyncRead for Transport {
  fn poll_read(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    match &mut *self {
      #[cfg(unix)]
      Self::Local(stream) => Pin::new(stream).poll_read(context, buffer),
      Self::Ssh(stream) => Pin::new(stream).poll_read(context, buffer),
    }
  }
}

impl AsyncWrite for Transport {
  fn poll_write(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &[u8],
  ) -> Poll<io::Result<usize>> {
    match &mut *self {
      #[cfg(unix)]
      Self::Local(stream) => Pin::new(stream).poll_write(context, buffer),
      Self::Ssh(stream) => Pin::new(stream).poll_write(context, buffer),
    }
  }

  fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut *self {
      #[cfg(unix)]
      Self::Local(stream) => Pin::new(stream).poll_flush(context),
      Self::Ssh(stream) => Pin::new(stream).poll_flush(context),
    }
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut *self {
      #[cfg(unix)]
      Self::Local(stream) => Pin::new(stream).poll_shutdown(context),
      Self::Ssh(stream) => Pin::new(stream).poll_shutdown(context),
    }
  }
}

/// Opens a raw protocol stream for the selected local or SSH target.
///
/// # Errors
///
/// Returns an error when the local daemon cannot be connected or started, or
/// when the OpenSSH remote-command channel cannot be established.
pub async fn open_transport(target: &ConnectionTarget) -> Result<Transport, CoreError> {
  match target {
    ConnectionTarget::Local { socket_path } => {
      #[cfg(unix)]
      {
        Ok(Transport::Local(
          rmux_ipc::connect_or_start_daemon(socket_path).await?,
        ))
      }
      #[cfg(not(unix))]
      {
        let _ = socket_path;
        Err(CoreError::LocalTransportUnsupported)
      }
    }
    ConnectionTarget::Ssh { destination } => {
      Ok(Transport::Ssh(open_ssh_tunnel(destination).await?))
    }
  }
}

/// One OpenSSH remote-command channel carrying raw `rmux-proto` bytes.
///
/// Dropping the stream closes its pipes and asks the supervisor to terminate
/// and reap the SSH child. A fresh reconnect always creates a fresh SSH
/// channel; OpenSSH may transparently reuse a configured control master.
pub struct SshTransport {
  stdin: ChildStdin,
  stdout: ChildStdout,
  shutdown: watch::Sender<bool>,
}

impl AsyncRead for SshTransport {
  fn poll_read(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.stdout).poll_read(context, buffer)
  }
}

impl AsyncWrite for SshTransport {
  fn poll_write(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &[u8],
  ) -> Poll<Result<usize, io::Error>> {
    Pin::new(&mut self.stdin).poll_write(context, buffer)
  }

  fn poll_flush(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
  ) -> Poll<Result<(), io::Error>> {
    Pin::new(&mut self.stdin).poll_flush(context)
  }

  fn poll_shutdown(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
  ) -> Poll<Result<(), io::Error>> {
    Pin::new(&mut self.stdin).poll_shutdown(context)
  }
}

impl Drop for SshTransport {
  fn drop(&mut self) {
    let _ignored = self.shutdown.send(true);
  }
}

/// Starts `ctld connect` through the system OpenSSH client.
///
/// The destination is interpreted exactly as an OpenSSH destination or
/// `~/.ssh/config` host alias. No shell fragment or user-controlled remote
/// command is accepted. SSH diagnostics and remote `ctld` diagnostics remain
/// on stderr and can never corrupt the protocol stream.
///
/// # Errors
///
/// Returns an error when the destination is unsafe or OpenSSH cannot be
/// started with piped stdin/stdout.
pub async fn open_ssh_tunnel(destination: &str) -> Result<SshTransport, CoreError> {
  validate_destination(destination)?;
  let mut command = Command::new(SSH_PROGRAM);
  command
    .args(ssh_arguments(destination))
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .kill_on_drop(true);

  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let (shutdown, mut shutdown_requested) = watch::channel(false);

  tokio::spawn(async move {
    tokio::select! {
      result = child.wait() => {
        if let Err(error) = result {
          eprintln!("ctl: could not wait for ssh: {error}");
        }
      }
      changed = shutdown_requested.changed() => {
        if changed.is_ok() && *shutdown_requested.borrow() {
          let _ignored = child.start_kill();
        }
        if let Err(error) = child.wait().await {
          eprintln!("ctl: could not reap ssh: {error}");
        }
      }
    }
  });

  let mut transport = SshTransport {
    stdin,
    stdout,
    shutdown,
  };
  let mut preface = vec![0_u8; SSH_TRANSPORT_PREFACE.len()];
  transport
    .read_exact(&mut preface)
    .await
    .map_err(CoreError::ReadSshPreface)?;
  if preface != SSH_TRANSPORT_PREFACE {
    return Err(CoreError::InvalidSshPreface);
  }
  Ok(transport)
}

/// Returns whether opening a replacement transport may succeed without a
/// configuration change.
#[must_use]
pub fn is_retryable_connection_error(error: &CoreError) -> bool {
  match error {
    #[cfg(unix)]
    CoreError::LocalIpc(source) => source.is_endpoint_unavailable(),
    CoreError::ReadSshPreface(source) => !matches!(
      source.kind(),
      io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ),
    CoreError::InvalidSshDestination(_)
    | CoreError::StartSsh(_)
    | CoreError::MissingSshStdin
    | CoreError::MissingSshStdout
    | CoreError::InvalidSshPreface => false,
    #[cfg(not(unix))]
    CoreError::LocalTransportUnsupported => false,
  }
}

fn validate_destination(destination: &str) -> Result<(), CoreError> {
  if destination.trim().is_empty() || destination.chars().any(char::is_control) {
    return Err(CoreError::InvalidSshDestination(destination.into()));
  }
  Ok(())
}

fn ssh_arguments(destination: &str) -> Vec<OsString> {
  [
    "-T",
    "-o",
    "ClearAllForwardings=yes",
    "-o",
    "ForwardAgent=no",
    "-o",
    "ForwardX11=no",
    "-o",
    "PermitLocalCommand=no",
    "-o",
    "RemoteCommand=none",
    "--",
    destination,
    REMOTE_COMMAND[0],
    REMOTE_COMMAND[1],
    REMOTE_COMMAND[2],
  ]
  .into_iter()
  .map(OsString::from)
  .collect()
}

#[derive(Debug, Error)]
pub enum CoreError {
  #[cfg(unix)]
  #[error(transparent)]
  LocalIpc(#[from] rmux_ipc::ConnectError),
  #[cfg(not(unix))]
  #[error("local ctl transport is not implemented on this platform")]
  LocalTransportUnsupported,
  #[error("invalid SSH destination '{0}'")]
  InvalidSshDestination(String),
  #[error("could not start the system ssh client: {0}")]
  StartSsh(#[source] io::Error),
  #[error("the ssh client did not expose a writable stdin pipe")]
  MissingSshStdin,
  #[error("the ssh client did not expose a readable stdout pipe")]
  MissingSshStdout,
  #[error("could not read the ctld transport marker from SSH: {0}")]
  ReadSshPreface(#[source] io::Error),
  #[error(
    "remote stdout did not begin with the ctld transport marker; check non-interactive shell startup output"
  )]
  InvalidSshPreface,
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(unix)]
  use tokio::io::AsyncWriteExt;

  #[test]
  fn ssh_command_uses_a_fixed_remote_command_and_disables_forwarding() {
    assert_eq!(
      ssh_arguments("workstation"),
      [
        "-T",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "RemoteCommand=none",
        "--",
        "workstation",
        "exec",
        "ctld",
        "connect",
      ]
      .map(OsString::from)
    );
  }

  #[test]
  fn unsafe_destinations_are_rejected_before_starting_ssh() {
    assert!(validate_destination("").is_err());
    assert!(validate_destination("host\ncommand").is_err());
    assert!(validate_destination("user@host").is_ok());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn local_target_uses_the_existing_owner_endpoint_without_ssh() {
    let directory =
      std::env::temp_dir().join(format!("ctl-core-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).unwrap();
    let socket_path = directory.join("rmux.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut request = [0_u8; 4];
      stream.read_exact(&mut request).await.unwrap();
      assert_eq!(&request, b"ping");
      stream.write_all(b"pong").await.unwrap();
    });

    let target = ConnectionTarget::Local {
      socket_path: socket_path.clone(),
    };
    let mut transport = open_transport(&target).await.unwrap();
    transport.write_all(b"ping").await.unwrap();
    let mut response = [0_u8; 4];
    transport.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, b"pong");

    server.await.unwrap();
    drop(transport);
    std::fs::remove_file(socket_path).unwrap();
    std::fs::remove_dir(directory).unwrap();
  }
}
