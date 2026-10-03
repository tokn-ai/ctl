//! Local and OpenSSH transport primitives for `ctl`.
//!
//! Local connections use owner-only daemon endpoints. Remote
//! authentication, host verification, proxying, and connection multiplexing
//! belong to the user's OpenSSH installation and configuration.

use std::ffi::OsString;
use std::future::{Future, ready};
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::watch;

pub mod hosts;
pub mod maintenance;
pub mod remote_bundle;
pub mod setup;
mod ssh_install;
pub mod ssh_reachability;
mod ssh_startup;
pub mod tailscale;

pub use ssh_install::{
  RemoteInstallEvent, RemoteInstallPhase, RemoteInstallProgress, RemoteInstallStalled,
  RemoteInstallWatchdog, install_ssh_unix_agent_interactive,
  install_ssh_unix_agent_interactive_with_progress,
};

const SSH_PROGRAM: &str = "ssh";
const MAX_SSH_COMMAND_OUTPUT: usize = 8192;
const UNIX_GATEWAY_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; }; "#,
  "exec ctl-agent connect",
);
const UNIX_AUTHENTICATED_GATEWAY_COMMAND: &str = concat!(
  r#"printf 'ctl-ssh-auth-v1\n'; PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; }; "#,
  "exec ctl-agent connect",
);
const UNIX_PLATFORM_PROBE_COMMAND: &str = "printf 'ctl-platform-v1\\n'; uname -s; uname -m";
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
const SSH_AUTHENTICATED_PREFACE: &[u8] = b"ctl-ssh-auth-v1\n";
const SSH_AGENT_NOT_FOUND_PREFACE: &[u8] = b"ctl-ssh-nf\n";

/// The fixed per-user service exposed through an SSH gateway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RemoteService {
  #[default]
  Ctmux,
  Task,
}

/// The daemon endpoint selected for one `ctl` operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionTarget {
  /// The current user's owner-only local `ctmuxd` endpoint.
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
  pub gateways: Vec<SshGateway>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshGatewayMode {
  Automatic,
  NativeOnly,
  AgentRelayOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshGateway {
  pub kind: ctl_ipc::GatewayKind,
  pub vpn: Option<ctl_ipc::VpnGateway>,
  pub destination: String,
  pub hostname: Option<String>,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<PathBuf>,
  pub mode: SshGatewayMode,
}

impl SshGateway {
  fn to_ipc(&self) -> ctl_ipc::SshGateway {
    ctl_ipc::SshGateway {
      kind: self.kind,
      vpn: self.vpn.clone(),
      destination: self.destination.clone(),
      hostname: self.hostname.clone(),
      user: self.user.clone(),
      port: self.port,
      identity_file: self.identity_file.clone(),
      mode: match self.mode {
        SshGatewayMode::Automatic => ctl_ipc::SshGatewayMode::Automatic,
        SshGatewayMode::NativeOnly => ctl_ipc::SshGatewayMode::NativeOnly,
        SshGatewayMode::AgentRelayOnly => ctl_ipc::SshGatewayMode::AgentRelayOnly,
      },
    }
  }
}

/// Local prompt handling only; this cannot alter the remote command.
pub enum SshInteraction {
  Inherit,
  Batch,
  /// Reuses this exact master and fails if it is unavailable; never starts a
  /// separate SSH connection, even when noninteractive credentials are usable.
  Multiplexed {
    control_path: PathBuf,
  },
  Askpass {
    program: PathBuf,
    socket: PathBuf,
    token: String,
  },
}

impl ConnectionTarget {
  /// Selects the current user's default local `ctmuxd` endpoint.
  #[must_use]
  pub fn local() -> Self {
    Self::Local {
      socket_path: ctmux_ipc::socket_path(),
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
pub enum Transport<LocalStream = ctmux_ipc::Stream> {
  Local(LocalStream),
  Ssh(SshTransport),
}

/// A task protocol stream over the local task endpoint or OpenSSH.
pub type TaskTransport = Transport<ctl_task_ipc::Stream>;

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
  open_transport_with_interaction(target, &SshInteraction::Inherit).await
}

/// Opens a raw protocol stream with an explicit local SSH interaction policy.
///
/// Local targets ignore the interaction. SSH targets use it only for local
/// authentication and multiplex selection; the remote command remains fixed.
///
/// # Errors
/// Returns local daemon startup, SSH startup, or transport-marker failures.
pub async fn open_transport_with_interaction(
  target: &ConnectionTarget,
  interaction: &SshInteraction,
) -> Result<Transport, CoreError> {
  match target {
    ConnectionTarget::Local { socket_path } => Ok(Transport::Local(
      ctmux_ipc::connect_or_start_daemon(socket_path).await?,
    )),
    ConnectionTarget::Ssh {
      destination,
      options,
    } => Ok(Transport::Ssh(
      open_ssh_tunnel_interactive(destination, options, interaction).await?,
    )),
  }
}

/// Opens the selected user's task endpoint locally or through SSH.
///
/// A local target's socket path selects ctmux for interactive attachments;
/// task requests always use the current user's fixed task endpoint.
///
/// # Errors
/// Returns task daemon startup, SSH startup, or transport-marker failures.
pub async fn open_task_transport(target: &ConnectionTarget) -> Result<TaskTransport, CoreError> {
  open_task_transport_with_interaction(target, &SshInteraction::Inherit).await
}

/// Opens the selected task service with an explicit local SSH interaction policy.
///
/// # Errors
/// Returns task daemon startup, SSH startup, or transport-marker failures.
pub async fn open_task_transport_with_interaction(
  target: &ConnectionTarget,
  interaction: &SshInteraction,
) -> Result<TaskTransport, CoreError> {
  match target {
    ConnectionTarget::Local { .. } => Ok(Transport::Local(
      ctl_task_client::connect_or_start(&ctl_task_ipc::socket_path()).await?,
    )),
    ConnectionTarget::Ssh {
      destination,
      options,
    } => Ok(Transport::Ssh(
      open_ssh_service_interactive(destination, options, interaction, RemoteService::Task).await?,
    )),
  }
}

/// One OpenSSH remote-command channel carrying raw service protocol bytes.
///
/// Dropping the stream closes its pipes and asks the supervisor to terminate
/// and reap the SSH child. A fresh reconnect always creates a fresh SSH
/// channel; OpenSSH may transparently reuse a configured control master.
pub struct SshTransport {
  pub remote_identity: Option<Box<ctl_proto::RemoteIdentity>>,
  stdin: ChildStdin,
  stdout: BufReader<ChildStdout>,
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
  open_ssh_service_interactive(destination, options, interaction, RemoteService::Ctmux).await
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
  let arguments = ssh_service_arguments(destination, options, interaction, service).await?;
  let extra = configure_ssh_interaction(&mut command, interaction);
  command.args(extra).args(arguments);
  start_ssh_transport(command).await
}

/// Opens a service stream with identity metadata on the same SSH channel.
///
/// # Errors
/// Returns validation, startup, or identity protocol errors.
pub async fn open_identified_ssh_service(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
) -> Result<SshTransport, CoreError> {
  validate_ssh_target(destination, options)?;
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(ssh_service_arguments(destination, options, interaction, service).await?)
    .arg("--identity");
  start_ssh_transport_identified(command, true, false, ready(())).await
}

/// Reads legacy v2/v3 identity solely to verify the account before replacing its
/// incompatible components. No service requests are sent and no service stream
/// is returned. The disposable SSH channel is terminated after metadata reads.
///
/// # Errors
/// Returns validation, SSH startup, unsupported-marker, or bounded identity
/// errors. This compatibility probe never permits a v2 service connection.
pub async fn inspect_legacy_ssh_identity(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
) -> Result<ctl_proto::RemoteIdentity, CoreError> {
  validate_ssh_target(destination, options)?;
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(ssh_service_arguments(destination, options, interaction, service).await?)
    .arg("--identity");
  inspect_legacy_ssh_command(command).await
}

/// Opens an identified Unix service and pauses after SSH authentication.
///
/// The fixed remote command emits an authentication marker before attempting
/// to execute `ctl-agent`. The callback therefore runs after OpenSSH accepts
/// the connection even when the agent is absent, but before identity metadata
/// is read or verified.
///
/// # Errors
/// Returns validation, startup, or identity protocol errors.
pub async fn open_identified_ssh_service_after_authentication(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
  on_authenticated: impl Future<Output = ()>,
) -> Result<SshTransport, CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(ssh_authenticated_service_arguments(destination, options, interaction, service).await?)
    .arg("--identity");
  start_ssh_transport_identified(command, true, true, on_authenticated).await
}

fn configure_ssh_interaction(command: &mut Command, interaction: &SshInteraction) -> Vec<OsString> {
  match interaction {
    SshInteraction::Inherit => Vec::new(),
    SshInteraction::Batch => vec!["-o".into(), "BatchMode=yes".into()],
    SshInteraction::Multiplexed { control_path } => vec![
      "-S".into(),
      control_path.as_os_str().to_owned(),
      "-o".into(),
      "ControlMaster=no".into(),
      "-o".into(),
      "BatchMode=yes".into(),
      // ControlMaster=no alone falls back to a fresh SSH connection when the
      // socket disappears. An existing master bypasses ProxyCommand entirely.
      // Keep this before route options so no proxy can restore that fallback.
      "-o".into(),
      "ProxyCommand=false".into(),
      // These are owned, piped channel processes even when the master belongs
      // to another application. Never detach or discard their input via config.
      "-o".into(),
      "ForkAfterAuthentication=no".into(),
      "-o".into(),
      "StdinNull=no".into(),
    ],
    SshInteraction::Askpass {
      program,
      socket,
      token,
    } => {
      command
        .env("SSH_ASKPASS", program)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "ctmux-askpass")
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
  const MARKER: &[u8] = b"ctl-platform-v1\n";
  let command = ssh_command_interactive(
    destination,
    options,
    interaction,
    UNIX_PLATFORM_PROBE_COMMAND,
  )
  .await?;
  let output = run_marked_fixed_command(command, &[], MARKER).await?;
  let output = [MARKER, output.as_slice()].concat();
  String::from_utf8(output).map_err(|_| CoreError::InvalidSshCommandOutput)
}

/// Restarts only the account's ctmux daemon, after the caller confirms session loss.
/// The agent verifies the expected identity before accessing the control endpoint.
///
/// # Errors
/// Returns validation, SSH, remote restart, or invalid-response errors.
pub async fn restart_ssh_ctmux_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  expected_remote_id: &str,
) -> Result<ctl_proto::RemoteCtmuxRestartResult, CoreError> {
  const COMMAND: &str = concat!(
    r#"printf 'ctl-command-v1\n'; "#,
    r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
    "exec ctl-agent restart-ctmux",
  );
  let input = serde_json::to_vec(&ctl_proto::RemoteCtmuxRestartRequest {
    expected_remote_id: expected_remote_id.into(),
  })
  .map_err(|_| CoreError::InvalidSshCommandOutput)?;
  let command = ssh_command_interactive(destination, options, interaction, COMMAND).await?;
  let output = run_marked_fixed_command(command, &input, b"ctl-command-v1\n").await?;
  serde_json::from_slice(&output).map_err(|_| CoreError::InvalidSshCommandOutput)
}

async fn ssh_command_interactive(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  remote_command: &str,
) -> Result<Command, CoreError> {
  validate_ssh_target(destination, options)?;
  if options.remote_platform != RemotePlatform::Unix {
    return Err(CoreError::InvalidSshOption("remote_platform".into()));
  }
  let mut command = Command::new(SSH_PROGRAM);
  let extra = configure_ssh_interaction(&mut command, interaction);
  command
    .args(extra)
    .args(prepare_ssh_base_arguments(destination, options, interaction).await?)
    .arg(remote_command);
  Ok(command)
}

async fn run_marked_fixed_command(
  mut command: Command,
  input: &[u8],
  marker: &[u8],
) -> Result<Vec<u8>, CoreError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);

  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let mut stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let stdout = child.stdout.take().ok_or(CoreError::MissingSshStdout)?;
  let mut stderr = child.stderr.take().ok_or(CoreError::MissingSshStderr)?;
  let write = async move {
    let result = stdin.write_all(input).await;
    // ChildStdin::shutdown is a no-op on Unix. Close the owned pipe so the
    // remote command can observe EOF before we wait for its response/exit.
    drop(stdin);
    result
  };
  let read_stdout = async move {
    let mut stdout = BufReader::new(stdout);
    // Closing the owned pipe on framing failure also prevents the producer
    // from blocking on verbose startup output while we await its diagnostics.
    ssh_startup::Preface::default()
      .read_marker(&mut stdout, &[marker], marker)
      .await?;
    read_bounded_output(&mut stdout).await
  };
  let read_stderr = read_bounded_output(&mut stderr);
  let wait = child.wait();
  let (write, stdout, stderr, status) = tokio::join!(write, read_stdout, read_stderr, wait);
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
  write.map_err(CoreError::WriteSshCommand)?;
  stdout.map_err(CoreError::ReadSshCommand)
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

async fn start_ssh_transport(command: Command) -> Result<SshTransport, CoreError> {
  start_ssh_transport_identified(command, false, false, ready(())).await
}

struct SshStartup {
  child: tokio::process::Child,
  stdin: ChildStdin,
  stdout: BufReader<ChildStdout>,
  diagnostics: ssh_startup::Diagnostics,
}

fn spawn_ssh_command(mut command: Command) -> Result<SshStartup, CoreError> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let mut child = command.spawn().map_err(CoreError::StartSsh)?;
  let stdin = child.stdin.take().ok_or(CoreError::MissingSshStdin)?;
  let stdout = BufReader::new(child.stdout.take().ok_or(CoreError::MissingSshStdout)?);
  let diagnostics = ssh_startup::Diagnostics::start(child.stderr.take());
  Ok(SshStartup {
    child,
    stdin,
    stdout,
    diagnostics,
  })
}

async fn inspect_legacy_ssh_command(
  command: Command,
) -> Result<ctl_proto::RemoteIdentity, CoreError> {
  const LEGACY_PREFACES: &[&[u8]] = &[b"ctl-ssh-v2\n", b"ctl-ssh-v3\n"];
  let SshStartup {
    mut child,
    stdin,
    mut stdout,
    diagnostics,
  } = spawn_ssh_command(command)?;
  // Keep stdin open until disposal, without ever writing a service frame. This
  // prevents the legacy relay from closing before it has emitted its identity.
  let _stdin = stdin;
  let probe = async {
    ssh_startup::Preface::default()
      .read_marker(&mut stdout, LEGACY_PREFACES, b"ctl-ssh-")
      .await?;
    Ok::<_, io::Error>(ctl_proto::read_identity(&mut stdout).await)
  };
  let result = match tokio::time::timeout(std::time::Duration::from_secs(30), probe).await {
    Ok(Ok(identity)) => identity.map_err(CoreError::RemoteIdentity),
    Ok(Err(error)) => {
      return Err(ssh_startup::startup_error(child, diagnostics, error).await);
    }
    Err(_) => Err(CoreError::RemoteIdentity(io::Error::new(
      io::ErrorKind::TimedOut,
      "legacy remote identity inspection timed out",
    ))),
  };
  let _ = child.start_kill();
  let reaped = child.wait().await;
  drop(diagnostics);
  let identity = result?;
  reaped.map_err(CoreError::WaitSshCommand)?;
  Ok(identity)
}

async fn start_ssh_transport_identified<F>(
  command: Command,
  identified: bool,
  authentication_marker: bool,
  on_authenticated: F,
) -> Result<SshTransport, CoreError>
where
  F: Future<Output = ()>,
{
  let SshStartup {
    mut child,
    mut stdin,
    mut stdout,
    diagnostics,
  } = spawn_ssh_command(command)?;
  let mut preface = ssh_startup::Preface::default();
  let markers = [
    SSH_AUTHENTICATED_PREFACE,
    SSH_TRANSPORT_PREFACE,
    ctl_proto::IDENTITY_PREFACE,
    SSH_AGENT_NOT_FOUND_PREFACE,
  ];
  let startup = async {
    if authentication_marker {
      if preface
        .read_marker(&mut stdout, &markers, b"ctl-ssh-")
        .await?
        != 0
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "remote command skipped the SSH authentication marker",
        ));
      }
      on_authenticated.await;
    }
    preface
      .read_marker(&mut stdout, &markers, b"ctl-ssh-")
      .await
  }
  .await;
  let marker = match startup {
    Ok(index) => markers[index],
    Err(error) => return Err(ssh_startup::startup_error(child, diagnostics, error).await),
  };
  let expected = if identified {
    ctl_proto::IDENTITY_PREFACE
  } else {
    SSH_TRANSPORT_PREFACE
  };
  if marker == SSH_AGENT_NOT_FOUND_PREFACE {
    let _ = child.kill().await;
    drop(diagnostics);
    return Err(CoreError::AgentNotFound);
  }
  if identified && marker == SSH_TRANSPORT_PREFACE {
    return Err(CoreError::IdentityUnsupported);
  }
  if marker != expected {
    return Err(
      ssh_startup::startup_error(
        child,
        diagnostics,
        io::Error::new(
          io::ErrorKind::InvalidData,
          "remote command returned an unexpected transport marker",
        ),
      )
      .await,
    );
  }
  let remote_identity = if identified {
    let identity = match read_negotiated_ssh_identity(&mut stdout, &mut stdin).await {
      Ok(identity) => identity,
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
        return Err(ssh_startup::startup_error(child, diagnostics, error).await);
      }
      Err(error) => return Err(CoreError::RemoteIdentity(error)),
    };
    Some(Box::new(identity))
  } else {
    None
  };
  let (shutdown, shutdown_requested) = watch::channel(false);
  ssh_startup::supervise(child, diagnostics, shutdown_requested);

  Ok(SshTransport {
    remote_identity,
    stdin,
    stdout,
    shutdown,
  })
}

async fn read_negotiated_ssh_identity(
  stdout: &mut (impl AsyncRead + Unpin),
  stdin: &mut (impl AsyncWrite + Unpin),
) -> io::Result<ctl_proto::RemoteIdentity> {
  let selected = tokio::time::timeout(
    std::time::Duration::from_secs(30),
    ctl_proto::negotiate_identity_contract(stdout, stdin),
  )
  .await
  .map_err(|_| {
    io::Error::new(
      io::ErrorKind::TimedOut,
      "identity contract negotiation timed out",
    )
  })??;
  let identity = ctl_proto::read_identity(stdout).await?;
  if !identity
    .protocols
    .iter()
    .any(|protocol| protocol.name == "ctl_identity" && protocol.supports(selected))
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "remote identity metadata does not support the selected contract",
    ));
  }
  Ok(identity)
}

/// Returns whether opening a replacement transport may succeed without a
/// configuration change.
#[must_use]
pub fn is_retryable_connection_error(error: &CoreError) -> bool {
  match error {
    CoreError::LocalIpc(source) => source.is_endpoint_unavailable(),
    CoreError::LocalTask(ctl_task_client::ClientError::Connect(source)) => matches!(
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
    CoreError::LocalConnection(_)
    | CoreError::LocalTask(_)
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
    | CoreError::InvalidSshPreface(_)
    | CoreError::AgentNotFound
    | CoreError::IdentityUnsupported
    | CoreError::UnsupportedSshProtocol { .. }
    | CoreError::RemoteIdentity(_) => false,
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
  let route: Vec<_> = options.gateways.iter().map(SshGateway::to_ipc).collect();
  if !ctl_ipc::has_valid_gateway_route(&route) {
    return Err(CoreError::InvalidSshOption("VPN gateway".into()));
  }
  for gateway in &options.gateways {
    validate_destination(&gateway.destination)?;
    if gateway.mode == SshGatewayMode::AgentRelayOnly {
      return Err(CoreError::InvalidSshOption(
        "agent_relay_only requires managed relay support".into(),
      ));
    }
    if gateway
      .destination
      .chars()
      .any(|value| matches!(value, ',' | '@'))
      || gateway.hostname.as_ref().is_some_and(|value| {
        value.trim().is_empty()
          || value.chars().any(|value| matches!(value, ',' | '@'))
          || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
      })
      || gateway.user.as_ref().is_some_and(|value| {
        value.trim().is_empty()
          || value.chars().any(|value| matches!(value, ',' | '@'))
          || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
      })
      || gateway.port == Some(0)
    {
      return Err(CoreError::InvalidSshOption("gateway".into()));
    }
    if gateway.kind == ctl_ipc::GatewayKind::Socks5 && gateway.port.is_none() {
      return Err(CoreError::InvalidSshOption("SOCKS5 gateway port".into()));
    }
    if gateway.identity_file.is_some() {
      return Err(CoreError::InvalidSshOption(
        "gateway identity_file requires managed relay support; configure it in OpenSSH for native jumping"
          .into(),
      ));
    }
  }
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
async fn ssh_arguments(destination: &str, options: &SshConnectionOptions) -> Vec<OsString> {
  ssh_service_arguments(
    destination,
    options,
    &SshInteraction::Inherit,
    RemoteService::Ctmux,
  )
  .await
  .unwrap()
}

async fn ssh_service_arguments(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
) -> Result<Vec<OsString>, CoreError> {
  ssh_service_arguments_with_command(
    destination,
    options,
    interaction,
    options.remote_platform.command(),
    service,
  )
  .await
}

async fn ssh_authenticated_service_arguments(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  service: RemoteService,
) -> Result<Vec<OsString>, CoreError> {
  ssh_service_arguments_with_command(
    destination,
    options,
    interaction,
    &[UNIX_AUTHENTICATED_GATEWAY_COMMAND],
    service,
  )
  .await
}

async fn ssh_service_arguments_with_command(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
  command: &[&str],
  service: RemoteService,
) -> Result<Vec<OsString>, CoreError> {
  let mut arguments = prepare_ssh_base_arguments(destination, options, interaction).await?;
  append_service_arguments(&mut arguments, command, service);
  Ok(arguments)
}

fn append_service_arguments(
  arguments: &mut Vec<OsString>,
  command: &[&str],
  service: RemoteService,
) {
  arguments.extend(command.iter().map(OsString::from));
  if service == RemoteService::Task {
    arguments.extend([OsString::from("--service"), OsString::from("task")]);
  }
}

async fn prepare_ssh_base_arguments(
  destination: &str,
  options: &SshConnectionOptions,
  interaction: &SshInteraction,
) -> Result<Vec<OsString>, CoreError> {
  let fresh_route = !matches!(interaction, SshInteraction::Multiplexed { .. });
  let proxy = if fresh_route
    && options
      .gateways
      .iter()
      .any(|gateway| gateway.kind.requires_proxy_command())
  {
    let gateways = options
      .gateways
      .iter()
      .map(SshGateway::to_ipc)
      .collect::<Vec<_>>();
    Some(ctl_ipc::prepare_proxy_command(&gateways).await?)
  } else {
    None
  };
  Ok(ssh_base_arguments_with_proxy(
    destination,
    options,
    proxy,
    fresh_route,
  ))
}

fn ssh_base_arguments_with_proxy(
  destination: &str,
  options: &SshConnectionOptions,
  proxy: Option<String>,
  fresh_route: bool,
) -> Vec<OsString> {
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
  if let Some(proxy) = proxy {
    arguments.extend([
      OsString::from("-o"),
      OsString::from(format!("ProxyCommand={proxy}")),
      OsString::from("-o"),
      OsString::from("ControlPath=none"),
    ]);
  } else if fresh_route && !options.gateways.is_empty() {
    arguments.extend([
      // Unlike -J, this form respects an earlier ProxyCommand without treating
      // it as a conflicting argument. Multiplexed mode disables fresh routes.
      OsString::from("-o"),
      OsString::from(format!(
        "ProxyJump={}",
        options
          .gateways
          .iter()
          .map(gateway_jump_specification)
          .collect::<Vec<_>>()
          .join(","),
      )),
    ]);
  }
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

fn gateway_jump_specification(gateway: &SshGateway) -> String {
  let host = gateway.hostname.as_deref().unwrap_or(&gateway.destination);
  let host = if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
    format!("[{host}]")
  } else {
    host.to_owned()
  };
  format!(
    "{}{}{}",
    gateway
      .user
      .as_ref()
      .map(|user| format!("{user}@"))
      .unwrap_or_default(),
    host,
    gateway
      .port
      .map(|port| format!(":{port}"))
      .unwrap_or_default(),
  )
}

#[derive(Debug, Error)]
pub enum CoreError {
  #[error("ctl-agent is not installed on the remote host")]
  AgentNotFound,
  #[error("remote components do not support environment identity; update the remote components")]
  IdentityUnsupported,
  #[error(
    "remote ctl-agent uses incompatible transport marker {marker:?}; update the remote components (ctl-agent, ctmuxd, and ctl-taskd) to match this client"
  )]
  UnsupportedSshProtocol { marker: String },
  #[error("could not read remote identity: {0}")]
  RemoteIdentity(#[source] io::Error),
  #[error(transparent)]
  LocalIpc(#[from] ctmux_ipc::ConnectError),
  #[error(transparent)]
  LocalTask(#[from] ctl_task_client::ClientError),
  #[error(transparent)]
  LocalConnection(#[from] ctl_ipc::ConnectError),
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
  #[error("remote ctl-agent transport was not ready: {0}")]
  InvalidSshPreface(String),
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncWriteExt;

  #[tokio::test]
  async fn ssh_command_uses_a_fixed_remote_command_and_disables_forwarding() {
    assert_eq!(
      ssh_arguments("workstation", &SshConnectionOptions::default()).await,
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

  #[tokio::test]
  async fn windows_remote_command_is_fixed_and_independent_of_client_platform() {
    let options = SshConnectionOptions {
      remote_platform: RemotePlatform::Windows,
      ..SshConnectionOptions::default()
    };
    let arguments = ssh_arguments("windows-host", &options).await;
    assert_eq!(
      &arguments[arguments.len() - 4..],
      ["--", "windows-host", "ctl-agent.exe", "connect"].map(OsString::from)
    );
  }

  #[tokio::test]
  async fn mixed_gateway_route_uses_the_ordered_proxy_helper() {
    let options = SshConnectionOptions {
      gateways: vec![
        SshGateway {
          kind: ctl_ipc::GatewayKind::Socks5,
          vpn: None,
          destination: "proxy.internal".into(),
          hostname: None,
          user: None,
          port: Some(1080),
          identity_file: None,
          mode: SshGatewayMode::Automatic,
        },
        SshGateway {
          kind: ctl_ipc::GatewayKind::Ssh,
          vpn: None,
          destination: "bastion.internal".into(),
          hostname: None,
          user: None,
          port: None,
          identity_file: None,
          mode: SshGatewayMode::Automatic,
        },
      ],
      ..SshConnectionOptions::default()
    };
    let args = prepare_ssh_base_arguments("target.internal", &options, &SshInteraction::Inherit)
      .await
      .unwrap();
    let proxy = args
      .iter()
      .find(|arg| arg.to_string_lossy().starts_with("ProxyCommand="))
      .unwrap();
    let proxy = proxy.to_string_lossy();
    assert!(proxy.contains("--proxy-route"));
    assert!(
      !args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("ProxyJump="))
    );
    let encoded = proxy
      .split("--proxy-route ")
      .nth(1)
      .unwrap()
      .split(' ')
      .next()
      .unwrap();
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    assert_eq!(remainder, &[] as &[u8]);
    let bytes = pairs
      .iter()
      .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
      .collect::<Vec<_>>();
    let route: Vec<ctl_ipc::SshGateway> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(route[0].kind, ctl_ipc::GatewayKind::Socks5);
    assert_eq!(route[1].destination, "bastion.internal");
  }

  #[tokio::test]
  async fn managed_vpn_routes_use_a_private_proxy_and_reject_stale_endpoints() {
    let gateway = SshGateway {
      kind: ctl_ipc::GatewayKind::Vpn,
      vpn: Some(ctl_ipc::VpnGateway {
        connection_id: "saved-vpn".into(),
        socket_path: std::env::temp_dir().join("test-vpn-owner.sock"),
        expected_remote_id: None,
      }),
      destination: "saved-vpn".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    };
    let mut options = SshConnectionOptions {
      gateways: vec![gateway.clone()],
      ..SshConnectionOptions::default()
    };
    assert!(validate_ssh_target("target", &options).is_ok());
    let args = prepare_ssh_base_arguments("target", &options, &SshInteraction::Inherit)
      .await
      .unwrap();
    assert!(args.iter().any(|arg| arg == "ControlPath=none"));
    assert!(
      args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("ProxyCommand="))
    );
    assert!(
      !args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("ProxyJump="))
    );
    options.gateways[0].port = Some(1080);
    assert!(validate_ssh_target("target", &options).is_err());
    options.gateways = vec![gateway.clone(), gateway];
    assert!(validate_ssh_target("target", &options).is_err());
  }

  #[tokio::test]
  async fn ssh_command_preserves_the_order_and_endpoint_fields_of_native_gateways() {
    let options = SshConnectionOptions {
      gateways: vec![
        SshGateway {
          kind: ctl_ipc::GatewayKind::Ssh,
          vpn: None,
          destination: "edge-alias".into(),
          hostname: None,
          user: None,
          port: None,
          identity_file: None,
          mode: SshGatewayMode::Automatic,
        },
        SshGateway {
          kind: ctl_ipc::GatewayKind::Ssh,
          vpn: None,
          destination: "internal-alias".into(),
          hostname: Some("2001:db8::2".into()),
          user: Some("operator".into()),
          port: Some(2222),
          identity_file: None,
          mode: SshGatewayMode::NativeOnly,
        },
      ],
      ..SshConnectionOptions::default()
    };

    let arguments = ssh_arguments("server", &options).await;
    let jump = arguments
      .windows(2)
      .find(|pair| pair[0] == "-o" && pair[1].to_string_lossy().starts_with("ProxyJump="))
      .expect("native jump arguments");
    assert_eq!(jump[1], "ProxyJump=edge-alias,operator@[2001:db8::2]:2222");
  }

  #[tokio::test]
  async fn task_service_only_appends_fixed_arguments_on_either_remote_platform() {
    for remote_platform in [RemotePlatform::Unix, RemotePlatform::Windows] {
      let options = SshConnectionOptions {
        remote_platform,
        port: Some(2222),
        ..SshConnectionOptions::default()
      };
      let ctmux = ssh_service_arguments(
        "host",
        &options,
        &SshInteraction::Inherit,
        RemoteService::Ctmux,
      )
      .await
      .unwrap();
      let task = ssh_service_arguments(
        "host",
        &options,
        &SshInteraction::Inherit,
        RemoteService::Task,
      )
      .await
      .unwrap();
      assert_eq!(&task[..ctmux.len()], ctmux.as_slice());
      assert_eq!(
        &task[ctmux.len()..],
        ["--service", "task"].map(OsString::from)
      );
    }
  }

  #[tokio::test]
  async fn authenticated_service_uses_a_fixed_marker_before_the_unix_gateway() {
    let options = SshConnectionOptions::default();
    let arguments = ssh_authenticated_service_arguments(
      "host",
      &options,
      &SshInteraction::Inherit,
      RemoteService::Ctmux,
    )
    .await
    .unwrap();
    assert_eq!(
      &arguments[arguments.len() - 3..],
      ["--", "host", UNIX_AUTHENTICATED_GATEWAY_COMMAND].map(OsString::from)
    );
  }

  #[test]
  fn unsafe_destinations_are_rejected_before_starting_ssh() {
    assert!(validate_destination("").is_err());
    assert!(validate_destination("host\ncommand").is_err());
    assert!(validate_destination("user@host").is_ok());
  }

  #[tokio::test]
  async fn structured_ssh_settings_are_separate_arguments_before_the_destination() {
    let options = SshConnectionOptions {
      remote_platform: RemotePlatform::Unix,
      hostname: Some("127.0.0.1".into()),
      user: Some("ctmux".into()),
      port: Some(2222),
      identity_file: Some(PathBuf::from("/tmp/key with spaces")),
      gateways: Vec::new(),
    };
    let arguments = ssh_arguments("ctmux-remote-test", &options).await;

    assert!(validate_ssh_target("ctmux-remote-test", &options).is_ok());
    assert_eq!(
      &arguments[arguments.len() - 9..],
      [
        "-p",
        "2222",
        "-l",
        "ctmux",
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
  fn multiplexed_connections_require_the_selected_master_without_prompting() {
    let mut command = Command::new("ssh");
    let arguments = configure_ssh_interaction(
      &mut command,
      &SshInteraction::Multiplexed {
        control_path: PathBuf::from("/tmp/ctld/master"),
      },
    );

    assert_eq!(
      arguments,
      [
        "-S",
        "/tmp/ctld/master",
        "-o",
        "ControlMaster=no",
        "-o",
        "BatchMode=yes",
        "-o",
        "ProxyCommand=false",
        "-o",
        "ForkAfterAuthentication=no",
        "-o",
        "StdinNull=no",
      ]
      .map(OsString::from)
    );
  }

  #[test]
  fn managed_unix_gateway_precedes_the_legacy_path_without_user_input() {
    assert_eq!(
      UNIX_GATEWAY_COMMAND,
      concat!(
        r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
        r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; }; "#,
        "exec ctl-agent connect",
      )
    );
    assert!(!UNIX_GATEWAY_COMMAND.contains("workstation"));
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
      gateways: Vec::new(),
    };

    assert!(validate_ssh_target("label", &options).is_err());
  }

  #[tokio::test]
  async fn local_target_uses_the_existing_owner_endpoint_without_ssh() {
    let directory =
      std::env::temp_dir().join(format!("ctl-client-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    let socket_path = directory.join("ctmux.sock");
    #[cfg(unix)]
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    #[cfg(windows)]
    let socket_path = PathBuf::from(format!(r"\\.\pipe\ctl-client-{}", uuid::Uuid::new_v4()));
    #[cfg(windows)]
    let listener = ctmux_ipc::windows::Listener::bind(&socket_path).unwrap();
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
