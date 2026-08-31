//! OpenSSH transport primitives for `ctl`.
//!
//! Authentication, host verification, proxying, and connection multiplexing
//! belong to the user's OpenSSH installation and configuration. This crate
//! owns only one remote-command channel and exposes its stdin/stdout as the
//! bidirectional byte stream consumed by `rmux-client`.

use std::ffi::OsString;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

const SSH_PROGRAM: &str = "ssh";
const REMOTE_COMMAND: [&str; 3] = ["exec", "ctld", "connect"];
const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

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

/// Returns whether opening a replacement SSH channel may succeed without a
/// configuration change.
#[must_use]
pub fn is_retryable_connection_error(error: &CoreError) -> bool {
  match error {
    CoreError::ReadSshPreface(source) => !matches!(
      source.kind(),
      io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ),
    CoreError::InvalidSshDestination(_)
    | CoreError::StartSsh(_)
    | CoreError::MissingSshStdin
    | CoreError::MissingSshStdout
    | CoreError::InvalidSshPreface => false,
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
}
