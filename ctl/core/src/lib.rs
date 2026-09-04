//! Local and OpenSSH transport primitives for `ctl`.
//!
//! Local connections use the owner-only `rmuxd` endpoint. Remote
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
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

mod ssh_startup;

const SSH_PROGRAM: &str = "ssh";
const REMOTE_COMMAND: [&str; 3] = ["exec", "ctld", "connect"];
const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

/// The daemon endpoint selected for one `ctl` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
  /// The current user's owner-only local `rmuxd` endpoint.
  Local { socket_path: PathBuf },
  /// An OpenSSH destination or `Host` alias, optionally with app-local
  /// connection settings expressed as fixed command arguments.
  Ssh {
    destination: String,
    options: SshConnectionOptions,
  },
}

/// Non-secret OpenSSH connection settings supplied without parsing arbitrary
/// command-line options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshConnectionOptions {
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
}

/// Local prompt handling only; this cannot alter the remote command.
pub enum SshInteraction {
  Inherit,
  Batch,
  Askpass {
    program: PathBuf,
    socket: PathBuf,
    token: String,
  },
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
      options: SshConnectionOptions::default(),
    }
  }

  /// Selects an SSH destination with validated, structured connection
  /// settings. These settings never include forwarding or a remote command.
  #[must_use]
  pub fn ssh_with_options(destination: impl Into<String>, options: SshConnectionOptions) -> Self {
    Self::Ssh {
      destination: destination.into(),
      options,
    }
  }

  /// Returns a concise name suitable for user-facing status messages.
  #[must_use]
  pub fn label(&self) -> &str {
    match self {
      Self::Local { .. } => "local",
      Self::Ssh { destination, .. } => destination,
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
  Local(rmux_ipc::Stream),
  Ssh(SshTransport),
}

impl AsyncRead for Transport {
  fn poll_read(
    mut self: Pin<&mut Self>,
    context: &mut Context<'_>,
    buffer: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    match &mut *self {
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
      Self::Local(stream) => Pin::new(stream).poll_write(context, buffer),
      Self::Ssh(stream) => Pin::new(stream).poll_write(context, buffer),
    }
  }

  fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut *self {
      Self::Local(stream) => Pin::new(stream).poll_flush(context),
      Self::Ssh(stream) => Pin::new(stream).poll_flush(context),
    }
  }

  fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut *self {
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
    ConnectionTarget::Local { socket_path } => Ok(Transport::Local(
      rmux_ipc::connect_or_start_daemon(socket_path).await?,
    )),
    ConnectionTarget::Ssh {
      destination,
      options,
    } => Ok(Transport::Ssh(
      open_ssh_tunnel_with_options(destination, options).await?,
    )),
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
  open_ssh_tunnel_with_options(destination, &SshConnectionOptions::default()).await
}

async fn open_ssh_tunnel_with_options(
  destination: &str,
  options: &SshConnectionOptions,
) -> Result<SshTransport, CoreError> {
  open_ssh_tunnel_interactive(destination, options, &SshInteraction::Inherit).await
}

/// Opens the fixed ctld command with explicit local SSH prompt handling.
///
/// # Errors
/// Returns validation, SSH startup, or transport-marker failures.
pub async fn open_ssh_tunnel_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<SshTransport, CoreError> {
  validate_ssh_target(destination, options)?;
  let mut command = Command::new(SSH_PROGRAM);

  // Insert local-only options before `--`; never append them to the remote command.
  let arguments = ssh_arguments(destination, options);
  let extra: Vec<OsString> = match interaction {
    SshInteraction::Inherit => Vec::new(),
    SshInteraction::Batch => vec!["-o".into(), "BatchMode=yes".into()],
    SshInteraction::Askpass {
      program,
      socket,
      token,
    } => {
      command
        .env("SSH_ASKPASS", program)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "rmux-askpass")
        .env("CTL_SSH_ASKPASS", "1")
        .env("CTL_SSH_ASKPASS_SOCKET", socket)
        .env("CTL_SSH_ASKPASS_TOKEN", token);
      vec![
        "-o".into(),
        "BatchMode=no".into(),
        "-o".into(),
        "StrictHostKeyChecking=ask".into(),
      ]
    }
  };
  command.args(extra).args(arguments);
  start_ssh_transport(command).await
}

async fn start_ssh_transport(mut command: Command) -> Result<SshTransport, CoreError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let mut stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let diagnostics = ssh_startup::Diagnostics::start(child.stderr.take());
  let mut preface = vec![0_u8; SSH_TRANSPORT_PREFACE.len()];
  if let Err(error) = stdout.read_exact(&mut preface).await {
    return Err(ssh_startup::startup_error(child, diagnostics, error).await);
  }
  if preface != SSH_TRANSPORT_PREFACE {
    let _ = child.kill().await;
    drop(diagnostics);
    return Err(CoreError::InvalidSshPreface);
  }
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
    drop(diagnostics);
  });

  Ok(SshTransport {
    stdin,
    stdout,
    shutdown,
  })
}

/// Returns whether opening a replacement transport may succeed without a
/// configuration change.
#[must_use]
pub fn is_retryable_connection_error(error: &CoreError) -> bool {
  match error {
    CoreError::LocalIpc(source) => source.is_endpoint_unavailable(),
    CoreError::ReadSshPreface(source) => !matches!(
      source.kind(),
      io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ),
    // This is a preface-read failure enriched with stderr; retain its previous
    // reconnect behavior (for example after a transient connection refusal).
    CoreError::SshStartup(_) => true,
    CoreError::InvalidSshDestination(_)
    | CoreError::InvalidSshOption(_)
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

fn validate_ssh_target(destination: &str, options: &SshConnectionOptions) -> Result<(), CoreError> {
  validate_destination(destination)?;
  if let Some(hostname) = &options.hostname
    && (hostname.trim().is_empty()
      || hostname
        .chars()
        .any(|character| character.is_control() || character.is_whitespace()))
  {
    return Err(CoreError::InvalidSshOption("hostname".into()));
  }
  if let Some(user) = &options.user
    && (user.trim().is_empty()
      || user
        .chars()
        .any(|character| character.is_control() || character.is_whitespace()))
  {
    return Err(CoreError::InvalidSshOption("user".into()));
  }
  if options.port == Some(0) {
    return Err(CoreError::InvalidSshOption("port".into()));
  }
  if options
    .identity_file
    .as_ref()
    .is_some_and(|path| path.as_os_str().is_empty())
  {
    return Err(CoreError::InvalidSshOption("identity_file".into()));
  }
  Ok(())
}

fn ssh_arguments(destination: &str, options: &SshConnectionOptions) -> Vec<OsString> {
  let mut arguments = [
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
  ]
  .into_iter()
  .map(OsString::from)
  .collect::<Vec<_>>();
  if let Some(port) = options.port {
    arguments.extend([OsString::from("-p"), OsString::from(port.to_string())]);
  }
  if let Some(user) = &options.user {
    arguments.extend([OsString::from("-l"), OsString::from(user)]);
  }
  if let Some(identity_file) = &options.identity_file {
    arguments.extend([OsString::from("-i"), identity_file.as_os_str().to_owned()]);
  }
  arguments.extend([
    OsString::from("--"),
    OsString::from(options.hostname.as_deref().unwrap_or(destination)),
    OsString::from(REMOTE_COMMAND[0]),
    OsString::from(REMOTE_COMMAND[1]),
    OsString::from(REMOTE_COMMAND[2]),
  ]);
  arguments
}

#[derive(Debug, Error)]
pub enum CoreError {
  #[error(transparent)]
  LocalIpc(#[from] rmux_ipc::ConnectError),
  #[error("invalid SSH destination '{0}'")]
  InvalidSshDestination(String),
  #[error("invalid structured SSH setting '{0}'")]
  InvalidSshOption(String),
  #[error("could not start the system ssh client: {0}")]
  StartSsh(#[source] io::Error),
  #[error("the ssh client did not expose a writable stdin pipe")]
  MissingSshStdin,
  #[error("the ssh client did not expose a readable stdout pipe")]
  MissingSshStdout,
  #[error("could not read the ctld transport marker from SSH: {0}")]
  ReadSshPreface(#[source] io::Error),
  #[error("SSH connection failed before ctld was ready: {0}")]
  SshStartup(String),
  #[error(
    "remote stdout did not begin with the ctld transport marker; check non-interactive shell startup output"
  )]
  InvalidSshPreface,
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncWriteExt;

  #[test]
  fn ssh_command_uses_a_fixed_remote_command_and_disables_forwarding() {
    assert_eq!(
      ssh_arguments("workstation", &SshConnectionOptions::default()),
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

  #[test]
  fn structured_ssh_settings_are_separate_arguments_before_the_destination() {
    let options = SshConnectionOptions {
      hostname: Some("127.0.0.1".into()),
      user: Some("rmux".into()),
      port: Some(2222),
      identity_file: Some(PathBuf::from("/tmp/key with spaces")),
    };
    let arguments = ssh_arguments("rmux-remote-test", &options);

    assert!(validate_ssh_target("rmux-remote-test", &options).is_ok());
    assert_eq!(
      &arguments[arguments.len() - 11..],
      [
        "-p",
        "2222",
        "-l",
        "rmux",
        "-i",
        "/tmp/key with spaces",
        "--",
        "127.0.0.1",
        "exec",
        "ctld",
        "connect",
      ]
      .map(OsString::from)
    );
  }

  #[test]
  fn invalid_structured_ssh_settings_are_rejected() {
    let options = SshConnectionOptions {
      hostname: Some("host with spaces".into()),
      user: None,
      port: Some(0),
      identity_file: None,
    };

    assert!(validate_ssh_target("label", &options).is_err());
  }

  #[tokio::test]
  async fn local_target_uses_the_existing_owner_endpoint_without_ssh() {
    let directory =
      std::env::temp_dir().join(format!("ctl-core-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    let socket_path = directory.join("rmux.sock");
    #[cfg(unix)]
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    #[cfg(windows)]
    let socket_path = PathBuf::from(format!(r"\\.\pipe\ctl-core-{}", uuid::Uuid::new_v4()));
    #[cfg(windows)]
    let listener = rmux_ipc::windows::Listener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
      let mut stream = listener.accept().await.unwrap().0;
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
    #[cfg(unix)]
    std::fs::remove_file(socket_path).unwrap();
    std::fs::remove_dir(directory).unwrap();
  }
}

#[cfg(test)]
mod transport_tests;
