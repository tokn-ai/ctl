//! Local and OpenSSH transport primitives for `ctl`.
//!
//! Local connections use owner-only daemon endpoints. Remote
//! authentication, host verification, proxying, and connection multiplexing
//! belong to the user's OpenSSH installation and configuration.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

mod ssh_startup;

const SSH_PROGRAM: &str = "ssh";
const MAX_SSH_COMMAND_OUTPUT: usize = 8192;
const UNIX_GATEWAY_COMMAND: &str =
  r#"PATH="${XDG_DATA_HOME:-$HOME/.local/share}/ctl/current:$PATH" exec ctl-agent connect"#;
const UNIX_PLATFORM_PROBE_COMMAND: &str = "printf 'ctl-platform-v1\\n'; uname -s; uname -m";
const UNIX_INSTALL_COMMAND: &str = r#"set -eu
umask 077
base="${XDG_DATA_HOME:-$HOME/.local/share}/ctl"
versions="$base/versions"
destination="$versions/__BUNDLE_ID__"
temporary="$versions/.install-__BUNDLE_ID__-$$"
link="$base/.current-$$"
mkdir -p "$versions"
test ! -e "$temporary"
mkdir "$temporary"
trap 'rm -rf "$temporary" "$link"' EXIT HUP INT TERM
tar -xzf - -C "$temporary"
test -f "$temporary/ctl-agent"
test -f "$temporary/rmuxd"
test -f "$temporary/taskd"
chmod 700 "$temporary/ctl-agent" "$temporary/rmuxd" "$temporary/taskd"
if [ -e "$destination" ]; then
  rm -rf "$temporary"
else
  mv "$temporary" "$destination"
fi
test -x "$destination/ctl-agent"
test -x "$destination/rmuxd"
test -x "$destination/taskd"
ln -s "versions/__BUNDLE_ID__" "$link"
mv -f "$link" "$base/current"
trap - EXIT HUP INT TERM
printf 'ctl-install-v1\n'"#;
/// Remote command-shell convention, independent of the client platform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemotePlatform {
  #[default]
  Unix,
  /// Windows OpenSSH with its default cmd.exe shell.
  Windows,
}

impl RemotePlatform {
  fn command(self) -> &'static [&'static str] {
    match self {
      Self::Unix => &[UNIX_GATEWAY_COMMAND],
      Self::Windows => &["ctl-agent.exe", "connect"],
    }
  }
}
const SSH_TRANSPORT_PREFACE: &[u8] = b"ctl-ssh-v1\n";

/// The fixed per-user service exposed through an SSH gateway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteService {
  #[default]
  Rmux,
  Task,
}

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
  pub remote_platform: RemotePlatform,
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

/// A raw protocol stream over either a service's local endpoint or OpenSSH.
pub enum Transport<LocalStream = rmux_ipc::Stream> {
  Local(LocalStream),
  Ssh(SshTransport),
}

/// A task protocol stream over the local task endpoint or OpenSSH.
pub type TaskTransport = Transport<task_ipc::Stream>;

impl<LocalStream: AsyncRead + Unpin> AsyncRead for Transport<LocalStream> {
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

impl<LocalStream: AsyncWrite + Unpin> AsyncWrite for Transport<LocalStream> {
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

/// Opens the selected user's task endpoint locally or through SSH.
///
/// A local target's socket path selects rmux for interactive attachments;
/// task requests always use the current user's fixed task endpoint.
///
/// # Errors
/// Returns task daemon startup, SSH startup, or transport-marker failures.
pub async fn open_task_transport(target: &ConnectionTarget) -> Result<TaskTransport, CoreError> {
  match target {
    ConnectionTarget::Local { .. } => Ok(Transport::Local(
      task_client::connect_or_start(&task_ipc::socket_path()).await?,
    )),
    ConnectionTarget::Ssh {
      destination,
      options,
    } => Ok(Transport::Ssh(
      open_ssh_service_interactive(
        destination,
        options,
        &SshInteraction::Inherit,
        RemoteService::Task,
      )
      .await?,
    )),
  }
}

/// One OpenSSH remote-command channel carrying raw service protocol bytes.
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

/// Starts `ctl-agent connect` through the system OpenSSH client.
///
/// The destination is interpreted exactly as an OpenSSH destination or
/// `~/.ssh/config` host alias. No shell fragment or user-controlled remote
/// command is accepted. SSH diagnostics and remote `ctl-agent` diagnostics remain
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

/// Opens the fixed ctl-agent command with explicit local SSH prompt handling.
///
/// # Errors
/// Returns validation, SSH startup, or transport-marker failures.
pub async fn open_ssh_tunnel_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<SshTransport, CoreError> {
  open_ssh_service_interactive(destination, options, interaction, RemoteService::Rmux).await
}

/// Opens an enumerated gateway service with explicit local SSH prompt handling.
///
/// Service selection adds only a fixed argument pair to the remote command;
/// remote socket paths and arbitrary commands are never accepted.
///
/// # Errors
/// Returns validation, SSH startup, or transport-marker failures.
pub async fn open_ssh_service_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
) -> Result<SshTransport, CoreError> {
  validate_ssh_target(destination, options)?;
  let mut command = Command::new(SSH_PROGRAM);

  // Insert local-only options before `--`; never append them to the remote command.
  let arguments = ssh_service_arguments(destination, options, service);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command.args(extra).args(arguments);
  start_ssh_transport(command).await
}

fn configure_ssh_interaction(command: &mut Command, interaction: &SshInteraction) -> Vec<OsString> {
  match interaction {
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
  }
}

/// Detects the OS and architecture of a Unix SSH host with one fixed command.
///
/// The returned text starts with `ctl-platform-v1` followed by the `uname -s`
/// and `uname -m` values on separate lines. Arbitrary remote commands remain
/// unavailable to callers.
///
/// # Errors
/// Returns validation, SSH startup, remote-command, or output-limit failures.
pub async fn probe_ssh_unix_platform_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<String, CoreError> {
  let output = run_ssh_command_interactive(
    destination,
    options,
    interaction,
    UNIX_PLATFORM_PROBE_COMMAND,
    &[],
  )
  .await?;
  String::from_utf8(output).map_err(|_| CoreError::InvalidSshCommandOutput)
}

/// Installs one trusted ctl-agent bundle into the fixed per-user Unix location.
///
/// `bundle_id` is restricted to a path-safe immutable build identifier and the
/// archive is expanded by a fixed script. The public API cannot supply a remote
/// command or destination path.
///
/// # Errors
/// Returns validation, SSH startup, remote-command, or output failures.
pub async fn install_ssh_unix_agent_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  bundle_id: &str,
  archive: &[u8],
) -> Result<(), CoreError> {
  validate_agent_bundle_id(bundle_id)?;
  let script = UNIX_INSTALL_COMMAND.replace("__BUNDLE_ID__", bundle_id);
  let output =
    run_ssh_command_interactive(destination, options, interaction, &script, archive).await?;
  if output != b"ctl-install-v1\n" {
    return Err(CoreError::InvalidSshCommandOutput);
  }
  Ok(())
}

fn validate_agent_bundle_id(bundle_id: &str) -> Result<(), CoreError> {
  if bundle_id.is_empty()
    || bundle_id.len() > 128
    || !bundle_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
  {
    return Err(CoreError::InvalidAgentBundleId(bundle_id.into()));
  }
  Ok(())
}

async fn run_ssh_command_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  remote_command: &str,
  input: &[u8],
) -> Result<Vec<u8>, CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(ssh_base_arguments(destination, options))
    .arg(remote_command)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let mut stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let mut stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let mut stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let write = async {
    stdin.write_all(input).await?;
    stdin.shutdown().await
  };
  let read_stdout = read_bounded_output(&mut stdout);
  let read_stderr = read_bounded_output(&mut stderr);
  let wait = child.wait();
  let (write, stdout, stderr, status) = tokio::join!(write, read_stdout, read_stderr, wait);
  write.map_err(CoreError::WriteSshCommand)?;
  let stdout = stdout.map_err(CoreError::ReadSshCommand)?;
  let stderr = stderr.map_err(CoreError::ReadSshCommand)?;
  let status = status.map_err(CoreError::WaitSshCommand)?;
  if !status.success() {
    let diagnostic = String::from_utf8_lossy(&stderr)
      .chars()
      .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
      .collect::<String>()
      .trim()
      .to_owned();
    return Err(CoreError::SshCommandFailed {
      status: status.to_string(),
      diagnostic,
    });
  }
  Ok(stdout)
}

async fn read_bounded_output(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
  let mut retained = Vec::new();
  let mut buffer = [0_u8; 1024];
  loop {
    let count = reader.read(&mut buffer).await?;
    if count == 0 {
      return Ok(retained);
    }
    let keep = count.min(MAX_SSH_COMMAND_OUTPUT.saturating_sub(retained.len()));
    retained.extend_from_slice(&buffer[..keep]);
  }
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
    CoreError::LocalTask(task_client::ClientError::Connect(source)) => matches!(
      source.kind(),
      io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ),
    CoreError::ReadSshPreface(source) => !matches!(
      source.kind(),
      io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput | io::ErrorKind::PermissionDenied
    ),
    // This is a preface-read failure enriched with stderr; retain its previous
    // reconnect behavior (for example after a transient connection refusal).
    CoreError::SshStartup(_) => true,
    CoreError::LocalTask(_)
    | CoreError::InvalidSshDestination(_)
    | CoreError::InvalidSshOption(_)
    | CoreError::StartSsh(_)
    | CoreError::MissingSshStdin
    | CoreError::MissingSshStdout
    | CoreError::MissingSshStderr
    | CoreError::WriteSshCommand(_)
    | CoreError::ReadSshCommand(_)
    | CoreError::WaitSshCommand(_)
    | CoreError::SshCommandFailed { .. }
    | CoreError::InvalidSshCommandOutput
    | CoreError::InvalidAgentBundleId(_)
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

#[cfg(test)]
fn ssh_arguments(destination: &str, options: &SshConnectionOptions) -> Vec<OsString> {
  ssh_service_arguments(destination, options, RemoteService::Rmux)
}

fn ssh_service_arguments(
  destination: &str,
  options: &SshConnectionOptions,
  service: RemoteService,
) -> Vec<OsString> {
  let mut arguments = ssh_base_arguments(destination, options);
  arguments.extend(options.remote_platform.command().iter().map(OsString::from));
  if service == RemoteService::Task {
    arguments.extend([OsString::from("--service"), OsString::from("task")]);
  }
  arguments
}

fn ssh_base_arguments(destination: &str, options: &SshConnectionOptions) -> Vec<OsString> {
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
  ]);
  arguments
}

#[derive(Debug, Error)]
pub enum CoreError {
  #[error(transparent)]
  LocalIpc(#[from] rmux_ipc::ConnectError),
  #[error(transparent)]
  LocalTask(#[from] task_client::ClientError),
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
  #[error("the ssh client did not expose a readable stderr pipe")]
  MissingSshStderr,
  #[error("could not write the fixed SSH command input: {0}")]
  WriteSshCommand(#[source] io::Error),
  #[error("could not read the fixed SSH command output: {0}")]
  ReadSshCommand(#[source] io::Error),
  #[error("could not wait for the fixed SSH command: {0}")]
  WaitSshCommand(#[source] io::Error),
  #[error("fixed SSH command failed with {status}: {diagnostic}")]
  SshCommandFailed { status: String, diagnostic: String },
  #[error("fixed SSH command returned invalid output")]
  InvalidSshCommandOutput,
  #[error("invalid ctl-agent bundle id '{0}'")]
  InvalidAgentBundleId(String),
  #[error("could not read the ctl-agent transport marker from SSH: {0}")]
  ReadSshPreface(#[source] io::Error),
  #[error("SSH connection failed before ctl-agent was ready: {0}")]
  SshStartup(String),
  #[error(
    "remote stdout did not begin with the ctl-agent transport marker; check non-interactive shell startup output"
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
        UNIX_GATEWAY_COMMAND,
      ]
      .map(OsString::from)
    );
  }

  #[test]
  fn windows_remote_command_is_fixed_and_independent_of_client_platform() {
    let options = SshConnectionOptions {
      remote_platform: RemotePlatform::Windows,
      ..SshConnectionOptions::default()
    };
    let arguments = ssh_arguments("windows-host", &options);
    assert_eq!(
      &arguments[arguments.len() - 4..],
      ["--", "windows-host", "ctl-agent.exe", "connect"].map(OsString::from)
    );
  }

  #[test]
  fn task_service_only_appends_fixed_arguments_on_either_remote_platform() {
    for remote_platform in [RemotePlatform::Unix, RemotePlatform::Windows] {
      let options = SshConnectionOptions {
        remote_platform,
        port: Some(2222),
        ..SshConnectionOptions::default()
      };
      let rmux = ssh_service_arguments("host", &options, RemoteService::Rmux);
      let task = ssh_service_arguments("host", &options, RemoteService::Task);
      assert_eq!(&task[..rmux.len()], rmux.as_slice());
      assert_eq!(
        &task[rmux.len()..],
        ["--service", "task"].map(OsString::from)
      );
    }
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
      remote_platform: RemotePlatform::Unix,
      hostname: Some("127.0.0.1".into()),
      user: Some("rmux".into()),
      port: Some(2222),
      identity_file: Some(PathBuf::from("/tmp/key with spaces")),
    };
    let arguments = ssh_arguments("rmux-remote-test", &options);

    assert!(validate_ssh_target("rmux-remote-test", &options).is_ok());
    assert_eq!(
      &arguments[arguments.len() - 9..],
      [
        "-p",
        "2222",
        "-l",
        "rmux",
        "-i",
        "/tmp/key with spaces",
        "--",
        "127.0.0.1",
        UNIX_GATEWAY_COMMAND,
      ]
      .map(OsString::from)
    );
  }

  #[test]
  fn managed_unix_gateway_precedes_the_legacy_path_without_user_input() {
    assert_eq!(
      UNIX_GATEWAY_COMMAND,
      r#"PATH="${XDG_DATA_HOME:-$HOME/.local/share}/ctl/current:$PATH" exec ctl-agent connect"#
    );
    assert!(!UNIX_GATEWAY_COMMAND.contains("workstation"));
  }

  #[test]
  fn agent_bundle_ids_are_restricted_before_building_the_install_script() {
    for bundle_id in ["", "../escape", "v1/release", "line\nbreak"] {
      assert!(matches!(
        validate_agent_bundle_id(bundle_id),
        Err(CoreError::InvalidAgentBundleId(_))
      ));
    }
    assert!(validate_agent_bundle_id("0.1.0-dev.0123456789ab").is_ok());
  }

  #[cfg(unix)]
  #[test]
  fn unix_install_script_extracts_siblings_and_switches_the_managed_version() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let directory = std::env::temp_dir().join(format!(
      "ctl-core-install-{}",
      uuid::Uuid::new_v4().simple()
    ));
    let source = directory.join("source");
    let data = directory.join("data");
    let archive = directory.join("bundle.tar.gz");
    std::fs::create_dir_all(&source).unwrap();
    for binary in ["ctl-agent", "rmuxd", "taskd"] {
      std::fs::write(source.join(binary), binary).unwrap();
    }
    assert!(
      std::process::Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(&source)
        .args(["ctl-agent", "rmuxd", "taskd"])
        .status()
        .unwrap()
        .success()
    );

    let bundle_id = "0.1.0-dev.0123456789ab";
    let script = UNIX_INSTALL_COMMAND.replace("__BUNDLE_ID__", bundle_id);
    let mut child = std::process::Command::new("sh")
      .args(["-c", &script])
      .env("XDG_DATA_HOME", &data)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap();
    child
      .stdin
      .as_mut()
      .unwrap()
      .write_all(&std::fs::read(&archive).unwrap())
      .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ctl-install-v1\n");

    let installation = data.join("ctl/versions").join(bundle_id);
    assert_eq!(
      std::fs::read_link(data.join("ctl/current")).unwrap(),
      PathBuf::from("versions").join(bundle_id)
    );
    for binary in ["ctl-agent", "rmuxd", "taskd"] {
      let path = installation.join(binary);
      assert_eq!(std::fs::read_to_string(&path).unwrap(), binary);
      assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
      );
    }
    std::fs::remove_dir_all(directory).unwrap();
  }

  #[tokio::test]
  async fn unix_bootstrap_rejects_a_windows_target_before_starting_ssh() {
    let options = SshConnectionOptions {
      remote_platform: RemotePlatform::Windows,
      ..SshConnectionOptions::default()
    };
    assert!(matches!(
      probe_ssh_unix_platform_interactive("host", &options, &SshInteraction::Batch).await,
      Err(CoreError::InvalidSshOption(field)) if field == "remote_platform"
    ));
  }

  #[test]
  fn invalid_structured_ssh_settings_are_rejected() {
    let options = SshConnectionOptions {
      remote_platform: RemotePlatform::Unix,
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
