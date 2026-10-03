//! Narrow VPN control and byte streams through an authenticated SSH account.
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::task::{Context, Poll};
use std::time::Duration;

use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader, ReadBuf};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::{SshTarget, VpnConnection, VpnSnapshot, VpnStatus};

pub const PROTOCOL_BUILD: u16 = 1;
pub const CONTRACT_V1_0_1: ProtocolVersion = ProtocolVersion::new(1, 0, 1);
pub const PROTOCOL_VERSION: ProtocolVersion = CONTRACT_V1_0_1;
pub const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[CONTRACT_V1_0_1];
/// Stable framing marker; the published contract is negotiated separately.
pub const PREFACE: &[u8] = b"ctl-vpn\n";
const REMOTE_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-ssh-nf\n'; exit 127; }; "#,
  "exec ctl-agent vpn",
);
/// Shared startup budget for the SSH client and the remote agent's handshake.
/// Established byte streams have no startup or idle deadline.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolSelection {
  pub protocol_version: ProtocolVersion,
}

#[must_use]
pub fn protocol_offer() -> ProtocolOffer {
  ProtocolOffer::new(
    PROTOCOL_BUILD,
    PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
  )
}

/// Selects an explicitly implemented contract before identity or VPN input.
///
/// # Errors
/// Rejects malformed offers and peers without a common published contract.
pub async fn negotiate_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> Result<ProtocolVersion, Error> {
  let offer: ProtocolOffer = crate::read_frame(reader)
    .await?
    .ok_or(Error::ConnectionClosed("protocol offer"))?;
  let selected = offer
    .negotiate(SUPPORTED_PROTOCOL_VERSIONS)
    .ok_or(Error::UnsupportedProtocol)?;
  crate::write_frame(
    writer,
    &ProtocolSelection {
      protocol_version: selected,
    },
  )
  .await?;
  Ok(selected)
}

/// Advertises implemented contracts and verifies the client's explicit selection.
///
/// # Errors
/// Rejects malformed frames and selections absent from the published set.
pub async fn accept_contract(
  reader: &mut (impl AsyncRead + Unpin),
  writer: &mut (impl AsyncWrite + Unpin),
) -> Result<ProtocolVersion, Error> {
  let offer = protocol_offer();
  crate::write_frame(writer, &offer).await?;
  let selection: ProtocolSelection = crate::read_frame(reader)
    .await?
    .ok_or(Error::ConnectionClosed("protocol selection"))?;
  if !offer.accepts(selection.protocol_version) {
    return Err(Error::UnsupportedProtocol);
  }
  Ok(selection.protocol_version)
}

/// One request per authenticated channel. Connect switches to raw bytes.
/// Intentionally omits Debug because Start contains a password.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
  List,
  Start {
    connection: VpnConnection,
  },
  Stop {
    vpn_id: String,
  },
  Connect {
    connection_id: String,
    host: String,
    port: u16,
  },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
  Snapshot { snapshot: VpnSnapshot },
  Status { status: VpnStatus },
  Connected,
  Error { code: String, message: String },
}

impl Request {
  /// Validate without returning profile secrets or supplied addresses in errors.
  ///
  /// # Errors
  /// Returns a field-only diagnostic for an invalid request.
  pub fn validate(&self) -> Result<(), String> {
    match self {
      Self::List => Ok(()),
      Self::Start { connection } => connection.validate(),
      Self::Stop { vpn_id } => validate_id(vpn_id),
      Self::Connect {
        connection_id,
        host,
        port,
      } => {
        validate_id(connection_id)?;
        if host.is_empty()
          || host.len() > 255
          || host.chars().any(|c| c.is_control() || c.is_whitespace())
          || *port == 0
        {
          return Err("Invalid VPN TCP destination".into());
        }
        Ok(())
      }
    }
  }
}

fn validate_id(id: &str) -> Result<(), String> {
  if id.is_empty() || id.len() > 128 || id.chars().any(|c| c.is_control() || c.is_whitespace()) {
    Err("Invalid VPN connection ID".into())
  } else {
    Ok(())
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("remote VPN I/O failed: {0}")]
  Io(#[from] io::Error),
  #[error(transparent)]
  Codec(#[from] crate::CodecError),
  #[error(transparent)]
  Daemon(#[from] crate::ConnectError),
  #[error("remote VPN request is invalid: {0}")]
  InvalidRequest(String),
  #[error("remote VPN operation failed ({code}): {message}")]
  Remote { code: String, message: String },
  #[error("Remote identity changed; no VPN credentials or requests were sent")]
  IdentityMismatch,
  #[error("The remote agent does not support VPN control; update its components")]
  UnsupportedAgent,
  #[error("No shared published remote VPN contract; update the remote components")]
  UnsupportedProtocol,
  #[error("Remote VPN operation timed out")]
  Timeout,
  #[error("remote VPN SSH channel closed during {0}")]
  ConnectionClosed(&'static str),
  #[error("Unexpected remote VPN response")]
  UnexpectedResponse,
  #[error("Remote VPN SSH channel exited unsuccessfully")]
  SshFailed,
  #[error("remote VPN SSH master disappeared; reconnect its owner")]
  MasterUnavailable,
}

impl Error {
  /// Whether a fresh channel may recover after a network interruption.
  /// Identity, protocol, SSH authentication, and configuration errors are fatal.
  #[must_use]
  pub fn is_retryable_connection(&self) -> bool {
    match self {
      Self::Io(error) | Self::Codec(crate::CodecError::Io(error)) => {
        ctl_core::connection::is_transient_io_error(error)
      }
      Self::Timeout | Self::ConnectionClosed(_) | Self::MasterUnavailable => true,
      Self::Remote { code, .. } => matches!(
        code.as_str(),
        "request_timeout" | "vpn_connection_timeout" | "vpn_timeout"
      ),
      _ => false,
    }
  }
}

#[derive(Clone, Debug)]
pub struct Client {
  target: SshTarget,
  expected_remote_id: Option<String>,
  control_path: Option<PathBuf>,
  proxy_executable: Option<PathBuf>,
}

impl Client {
  #[must_use]
  pub fn new(target: SshTarget, expected_remote_id: Option<String>) -> Self {
    Self {
      target,
      expected_remote_id,
      control_path: None,
      proxy_executable: None,
    }
  }

  /// Pin to an existing master. Its disappearance cannot create a fresh SSH connection.
  #[must_use]
  pub fn with_control_path(mut self, control_path: PathBuf) -> Self {
    self.control_path = Some(control_path);
    self
  }

  /// Pin nested proxy routes to the already verified helper executable.
  #[must_use]
  pub fn with_proxy_executable(mut self, executable: PathBuf) -> Self {
    self.proxy_executable = Some(executable);
    self
  }

  /// # Errors
  /// Returns SSH, identity, protocol, or VPN operation failures.
  pub async fn list(&self) -> Result<VpnSnapshot, Error> {
    match self.request(Request::List, Duration::from_secs(30)).await? {
      Response::Snapshot { snapshot } => Ok(snapshot),
      _ => Err(Error::UnexpectedResponse),
    }
  }

  /// # Errors
  /// Returns SSH, identity, configuration, or VPN startup failures.
  pub async fn start_connection(&self, connection: VpnConnection) -> Result<VpnStatus, Error> {
    match self
      .request(Request::Start { connection }, Duration::from_secs(180))
      .await?
    {
      Response::Status { status } => Ok(status),
      _ => Err(Error::UnexpectedResponse),
    }
  }

  /// # Errors
  /// Returns SSH, identity, protocol, or targeted stop failures.
  pub async fn stop_id(&self, vpn_id: &str) -> Result<VpnStatus, Error> {
    match self
      .request(
        Request::Stop {
          vpn_id: vpn_id.into(),
        },
        Duration::from_secs(30),
      )
      .await?
    {
      Response::Status { status } => Ok(status),
      _ => Err(Error::UnexpectedResponse),
    }
  }

  /// Opens one TCP destination through the selected remote VPN, resolving DNS remotely.
  ///
  /// # Errors
  /// Returns SSH, identity, protocol, disconnected-VPN, or SOCKS connection failures.
  pub async fn open_connection(
    &self,
    connection_id: &str,
    host: &str,
    port: u16,
  ) -> Result<RemoteStream, Error> {
    let request = Request::Connect {
      connection_id: connection_id.into(),
      host: host.into(),
      port,
    };
    request.validate().map_err(Error::InvalidRequest)?;
    let mut stream = self.open().await?;
    match send_request(&mut stream, &request, Duration::from_secs(30)).await? {
      Response::Connected => Ok(stream),
      _ => Err(Error::UnexpectedResponse),
    }
  }

  async fn request(&self, request: Request, deadline: Duration) -> Result<Response, Error> {
    request.validate().map_err(Error::InvalidRequest)?;
    let mut stream = self.open().await?;
    let response = send_request(&mut stream, &request, deadline).await?;
    stream.finish().await?;
    Ok(response)
  }

  async fn open(&self) -> Result<RemoteStream, Error> {
    self.open_command(self.command().await?).await
  }

  async fn open_command(&self, mut command: Command) -> Result<RemoteStream, Error> {
    command
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::inherit())
      .kill_on_drop(true);
    let mut child = command.spawn()?;
    let input = child
      .stdin
      .take()
      .ok_or_else(|| io::Error::other("SSH stdin is missing"))?;
    let output = child
      .stdout
      .take()
      .ok_or_else(|| io::Error::other("SSH stdout is missing"))?;
    let (cancel, cancelled) = oneshot::channel();
    let waiter = tokio::spawn(async move {
      tokio::select! {
        result = child.wait() => result,
        _ = cancelled => { let _ = child.start_kill(); child.wait().await }
      }
    });
    let mut stream = RemoteStream {
      input: Some(input),
      output: BufReader::new(output),
      cancel: Some(cancel),
      waiter: Some(waiter),
      identity: None,
      protocol_version: None,
    };
    let (protocol_version, identity) = tokio::time::timeout(STARTUP_TIMEOUT, async {
      if let Err(error) = read_preface(&mut stream).await {
        if matches!(error, Error::UnsupportedAgent)
          && let Some(waiter) = stream.waiter.as_mut()
          && let Ok(Ok(Ok(status))) = tokio::time::timeout(Duration::from_secs(1), waiter).await
          && status.code() == Some(255)
        {
          // Failed SSH setup must not offer a component update. Older agents
          // reject the VPN command with exit 2; missing agents send a marker.
          return Err(
            if self
              .control_path
              .as_ref()
              .is_some_and(|path| path.try_exists().is_ok_and(|exists| !exists))
            {
              Error::MasterUnavailable
            } else {
              Error::SshFailed
            },
          );
        }
        return Err(error);
      }
      let input = stream.input.as_mut().expect("SSH input is present");
      let protocol_version = negotiate_contract(&mut stream.output, input).await?;
      let identity = ctl_proto::read_identity(&mut stream)
        .await
        .map_err(Error::Io)?;
      Ok::<_, Error>((protocol_version, identity))
    })
    .await
    .map_err(|_| Error::Timeout)??;
    if self
      .expected_remote_id
      .as_ref()
      .is_some_and(|expected| expected != &identity.remote_id)
    {
      stream.terminate().await;
      return Err(Error::IdentityMismatch);
    }
    stream.identity = Some(identity);
    stream.protocol_version = Some(protocol_version);
    Ok(stream)
  }

  async fn command(&self) -> Result<Command, Error> {
    validate_target(&self.target)?;
    let mut command = Command::new("ssh");
    command.args([
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
      "-o",
      "ForkAfterAuthentication=no",
      "-o",
      "StdinNull=no",
    ]);
    if let Some(control_path) = &self.control_path {
      command.arg("-S").arg(control_path).args([
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPersist=no",
        "-o",
        "BatchMode=yes",
        "-o",
        "ProxyCommand=false",
      ]);
    } else {
      command.args([
        "-o",
        "ControlPath=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPersist=no",
      ]);
      if !self.target.gateways.is_empty() {
        let proxy = if let Some(executable) = &self.proxy_executable {
          crate::proxy_command_with_executable(&self.target.gateways, executable)
        } else {
          crate::prepare_proxy_command(&self.target.gateways).await?
        };
        command.arg("-o").arg(format!("ProxyCommand={proxy}"));
      }
    }
    if let Some(port) = self.target.port {
      command.arg("-p").arg(port.to_string());
    }
    if let Some(user) = &self.target.user {
      command.arg("-l").arg(user);
    }
    if let Some(identity) = &self.target.identity_file {
      command.arg("-i").arg(identity);
    }
    if let Some(hostname) = &self.target.hostname {
      command.arg("-o").arg(format!("HostName={hostname}"));
    }
    command
      .arg("--")
      .arg(
        self
          .target
          .ssh_config_alias
          .as_deref()
          .unwrap_or(&self.target.destination),
      )
      .arg(REMOTE_COMMAND);
    Ok(command)
  }
}

fn validate_target(target: &SshTarget) -> Result<(), Error> {
  let valid = |value: &str| {
    !value.is_empty()
      && !value.starts_with('-')
      && !value.chars().any(|c| c.is_control() || c.is_whitespace())
  };
  if !valid(&target.destination)
    || target
      .ssh_config_alias
      .as_deref()
      .is_some_and(|value| !valid(value))
    || target
      .hostname
      .as_deref()
      .is_some_and(|value| !valid(value))
    || target.user.as_deref().is_some_and(|value| !valid(value))
    || target.port == Some(0)
    || !crate::has_valid_gateway_route(&target.gateways)
  {
    return Err(Error::InvalidRequest(
      "Invalid remote VPN SSH target".into(),
    ));
  }
  Ok(())
}

async fn send_request(
  stream: &mut RemoteStream,
  request: &Request,
  deadline: Duration,
) -> Result<Response, Error> {
  tokio::time::timeout(deadline, async {
    crate::write_frame(stream, request).await?;
    match crate::read_frame(stream).await? {
      Some(Response::Error { code, message }) => Err(Error::Remote { code, message }),
      Some(response) => Ok(response),
      None => Err(Error::ConnectionClosed("request response")),
    }
  })
  .await
  .map_err(|_| Error::Timeout)?
}

async fn read_preface(reader: &mut (impl AsyncRead + Unpin)) -> Result<(), Error> {
  let mut suffix = Vec::with_capacity(32);
  for _ in 0..64 * 1024 {
    let byte = match reader.read_u8().await {
      Ok(byte) => byte,
      Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
        return Err(Error::UnsupportedAgent);
      }
      Err(error) => return Err(error.into()),
    };
    suffix.push(byte);
    if suffix.ends_with(PREFACE) {
      return Ok(());
    }
    if suffix.ends_with(b"ctl-ssh-nf\n")
      || (byte == b'\n'
        && suffix
          .windows(b"ctl-vpn-".len())
          .any(|window| window == b"ctl-vpn-"))
    {
      return Err(Error::UnsupportedAgent);
    }
    if suffix.len() > 32 {
      suffix.remove(0);
    }
  }
  Err(Error::UnsupportedAgent)
}

/// A connection-owned SSH process. Dropping it kills and reaps that process.
pub struct RemoteStream {
  input: Option<ChildStdin>,
  output: BufReader<ChildStdout>,
  cancel: Option<oneshot::Sender<()>>,
  waiter: Option<JoinHandle<io::Result<ExitStatus>>>,
  identity: Option<ctl_proto::RemoteIdentity>,
  protocol_version: Option<ProtocolVersion>,
}

impl RemoteStream {
  /// Published contract selected for this channel, separate from advertisements.
  ///
  /// # Panics
  /// Panics if the private verified-stream construction invariant is violated.
  #[must_use]
  pub fn protocol_version(&self) -> ProtocolVersion {
    self
      .protocol_version
      .expect("contract is negotiated before returning a remote stream")
  }
  /// The authenticated account identity checked before opening this stream.
  ///
  /// # Panics
  /// Panics if the private verified-stream construction invariant is violated.
  #[must_use]
  pub fn identity(&self) -> &ctl_proto::RemoteIdentity {
    self
      .identity
      .as_ref()
      .expect("identity is verified before returning a remote stream")
  }

  async fn terminate(&mut self) {
    self.input.take();
    if let Some(cancel) = self.cancel.take() {
      let _ = cancel.send(());
    }
    if let Some(waiter) = self.waiter.take() {
      let _ = tokio::time::timeout(Duration::from_secs(3), waiter).await;
    }
  }

  async fn finish(mut self) -> Result<(), Error> {
    self.input.take();
    let waiter = self.waiter.take().expect("SSH waiter is present");
    match tokio::time::timeout(Duration::from_secs(5), waiter).await {
      Ok(result) => {
        let status = result.map_err(io::Error::other)??;
        if status.success() {
          Ok(())
        } else {
          Err(Error::SshFailed)
        }
      }
      Err(_) => Err(Error::Timeout),
    }
  }
}

impl Drop for RemoteStream {
  fn drop(&mut self) {
    if let Some(cancel) = self.cancel.take() {
      let _ = cancel.send(());
    }
  }
}

impl AsyncRead for RemoteStream {
  fn poll_read(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
  ) -> Poll<io::Result<()>> {
    Pin::new(&mut self.output).poll_read(cx, buf)
  }
}

impl AsyncWrite for RemoteStream {
  fn poll_write(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
    bytes: &[u8],
  ) -> Poll<io::Result<usize>> {
    match &mut self.input {
      Some(input) => Pin::new(input).poll_write(cx, bytes),
      None => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
    }
  }
  fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    match &mut self.input {
      Some(input) => Pin::new(input).poll_flush(cx),
      None => Poll::Ready(Ok(())),
    }
  }
  fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    // Tokio ChildStdin::shutdown is a no-op on Unix; dropping the pipe sends EOF.
    match self.as_mut().poll_flush(cx) {
      Poll::Ready(Ok(())) => {
        self.input.take();
        Poll::Ready(Ok(()))
      }
      result => result,
    }
  }
}

#[cfg(test)]
mod tests;
