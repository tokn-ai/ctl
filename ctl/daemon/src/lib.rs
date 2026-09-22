//! Per-user owner of authenticated OpenSSH control masters and SSH credentials.

#[cfg(target_os = "macos")]
mod keychain;
mod port_forwarding;
mod shared_forwarding;
mod ssh_config_master;
mod target_lifecycle;

#[cfg(test)]
mod master_policy_tests;

use ctld_ipc::{
  ClientMessage, LocalPortForward, PromptKind, ServerMessage, SshGateway, SshGatewayMode, SshTarget,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::time::{Instant, sleep};
use zeroize::Zeroizing;

use port_forwarding::{ForwardRegistry, SshForwardControl};
use shared_forwarding::SharedForwardRegistry;
use target_lifecycle::TargetLifecycle;

const SSH_PROGRAM: &str = "ssh";
const MASTER_IDLE_SECONDS: u64 = 300;
const MASTER_START_TIMEOUT: Duration = Duration::from_mins(3);
const MASTER_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MASTER_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DIAGNOSTICS: usize = 8192;
const MAX_LISTENER_CATALOG_BYTES: usize = 64 * 1024;
const LISTENER_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_LISTENERS_COMMAND: &str = concat!(
  r#"PATH="$HOME/.tokn/ctl/current:$PATH"; export PATH; "#,
  r#"command -v ctl-agent >/dev/null 2>&1 || { printf 'ctl-agent is not installed\n' >&2; exit 127; }; "#,
  "exec ctl-agent listeners",
);

#[derive(Default)]
struct State {
  attempts: Mutex<HashMap<String, Attempt>>,
  targets: Mutex<HashMap<String, Arc<TargetLifecycle>>>,
  forwards: AsyncMutex<ForwardRegistry>,
  configured_connections: Mutex<HashMap<String, ConnectionLease>>,
  shared_forwards: AsyncMutex<SharedForwardRegistry>,
}

#[derive(Clone, Debug)]
struct MasterEndpoint {
  control_path: PathBuf,
  shared: bool,
  startup: SharedMasterStartup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SharedMasterStartup {
  Create,
  PrivateFallback,
  ExternalOnly,
}

impl MasterEndpoint {
  fn managed(target: &SshTarget) -> Self {
    Self {
      control_path: control_path(target),
      shared: false,
      startup: SharedMasterStartup::PrivateFallback,
    }
  }

  /// A live reuse-only socket can disappear after config resolution. Retain
  /// the resolved creation policy instead of accidentally starting a new one.
  fn after_missing_master(self, target: &SshTarget) -> Result<Self, RequestError> {
    if !self.shared {
      return Ok(self);
    }
    match self.startup {
      SharedMasterStartup::Create => Ok(self),
      SharedMasterStartup::PrivateFallback => Ok(Self::managed(target)),
      SharedMasterStartup::ExternalOnly => Err(ssh_config_master::external_master_required()),
    }
  }
}

struct ConnectionLease {
  endpoint: MasterEndpoint,
  // Closing our anchor session lets OpenSSH apply ControlPersist. The shared
  // master may still serve channels owned by a terminal or another program.
  anchor: Option<ChildStdin>,
}

impl State {
  fn endpoint(&self, target: &SshTarget) -> Option<MasterEndpoint> {
    if target.ssh_config_alias.is_none() {
      return Some(MasterEndpoint::managed(target));
    }
    self
      .configured_connections
      .lock()
      .unwrap()
      .get(&target_key(target))
      .map(|lease| lease.endpoint.clone())
  }

  fn adopt(&self, target: &SshTarget, endpoint: &MasterEndpoint, anchor: Option<ChildStdin>) {
    if target.ssh_config_alias.is_some() {
      let mut connections = self.configured_connections.lock().unwrap();
      // Repeated Connect must not drop an existing nonpersistent anchor.
      if anchor.is_none()
        && connections.get(&target_key(target)).is_some_and(|lease| {
          lease.endpoint.control_path == endpoint.control_path
            && lease.endpoint.shared == endpoint.shared
            && lease.anchor.is_some()
        })
      {
        return;
      }
      connections.insert(
        target_key(target),
        ConnectionLease {
          endpoint: endpoint.clone(),
          anchor,
        },
      );
    }
  }

  fn target(&self, target: &SshTarget) -> Arc<TargetLifecycle> {
    Arc::clone(
      self
        .targets
        .lock()
        .unwrap()
        .entry(target_key(target))
        .or_default(),
    )
  }
}

struct Attempt {
  prompts: mpsc::Sender<PromptRequest>,
}

struct PromptRequest {
  message: String,
  confirm: bool,
  response: oneshot::Sender<Option<Zeroizing<String>>>,
}

struct AttemptGuard {
  token: String,
  state: Arc<State>,
}

impl Drop for AttemptGuard {
  fn drop(&mut self) {
    self.state.attempts.lock().unwrap().remove(&self.token);
  }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
  #[error("could not prepare ctld's runtime directory: {0}")]
  RuntimeDirectory(#[source] io::Error),
  #[error("could not bind ctld endpoint {}: {source}", path.display())]
  Bind { path: PathBuf, source: io::Error },
  #[error("could not accept a ctld connection: {0}")]
  Accept(#[source] io::Error),
  #[error("ctld is not supported on this platform")]
  UnsupportedPlatform,
}

#[derive(Debug, thiserror::Error)]
enum RequestError {
  #[error(transparent)]
  Codec(#[from] ctld_ipc::CodecError),
  #[error("ctld client ended the request")]
  ClientClosed,
  #[error("invalid ctld request: {0}")]
  InvalidRequest(&'static str),
  #[error(
    "ctld requires local protocol {expected}, but the client requested {actual}. Update the client and ctld together, then restart ctld."
  )]
  ProtocolVersionMismatch { expected: u16, actual: u16 },
  #[error("could not start the OpenSSH control master: {0}")]
  StartMaster(#[source] io::Error),
  #[error("OpenSSH control master timed out during authentication")]
  MasterTimeout,
  #[error("OpenSSH control master exited before becoming ready: {0}")]
  MasterFailed(String),
  #[error("could not use SSH configuration: {0}")]
  SshConfig(String),
  #[error("This SSH host was disconnected. Use Connect host to reconnect.")]
  HostDisconnected,
  #[error("could not disconnect the OpenSSH control master: {0}")]
  DisconnectFailed(String),
  #[error("OpenSSH port forwarding failed: {0}")]
  PortForwardFailed(String),
  #[error("remote listener discovery failed: {0}")]
  RemoteListenerFailed(String),
  #[error("remote components must be updated before TCP listeners can be discovered")]
  RemoteAgentUpdateRequired,
}

/// Runs the per-user broker until it is interrupted.
///
/// # Errors
/// Returns an error when the owner-only endpoint cannot be prepared or served.
#[cfg(unix)]
pub async fn run(socket_path: PathBuf) -> Result<(), DaemonError> {
  prepare_runtime_directory(&socket_path).map_err(DaemonError::RuntimeDirectory)?;
  let listener = bind_listener(&socket_path).await?;
  let _guard = SocketGuard(socket_path);
  let state = Arc::new(State::default());
  loop {
    tokio::select! {
      accepted = listener.accept() => {
        let (stream, _) = accepted.map_err(DaemonError::Accept)?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
          let _ = handle_connection(stream, state).await;
        });
      }
      result = tokio::signal::ctrl_c() => {
        result.map_err(DaemonError::Accept)?;
        return Ok(());
      }
    }
  }
}

#[cfg(not(unix))]
pub async fn run(_socket_path: PathBuf) -> Result<(), DaemonError> {
  Err(DaemonError::UnsupportedPlatform)
}

#[must_use]
pub fn askpass_exit_code() -> Option<i32> {
  if std::env::var("CTLD_ASKPASS").ok().as_deref() != Some("1") {
    return None;
  }
  let result = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .map_err(Into::into)
    .and_then(|runtime| runtime.block_on(run_askpass()));
  Some(i32::from(result.is_err()))
}

async fn run_askpass() -> Result<(), Box<dyn std::error::Error>> {
  let token = std::env::var("CTLD_ASKPASS_TOKEN")?;
  let message = std::env::args().nth(1).ok_or("missing askpass prompt")?;
  if message.len() > 8192 {
    return Err("askpass prompt is too long".into());
  }
  let confirm = std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref() == Some("confirm")
    || is_host_confirmation(&message);
  let mut stream = ctld_ipc::connect_existing().await?;
  handshake(&mut stream).await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::Askpass {
      token,
      message,
      confirm,
    },
  )
  .await?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
    Some(ServerMessage::AskpassResponse {
      response: Some(response),
    }) => {
      use tokio::io::AsyncWriteExt as _;
      let mut stdout = tokio::io::stdout();
      stdout.write_all(response.as_bytes()).await?;
      stdout.write_all(b"\n").await?;
      stdout.flush().await?;
      Ok(())
    }
    _ => Err("askpass request was cancelled".into()),
  }
}

#[cfg(unix)]
async fn handle_connection(
  mut stream: tokio::net::UnixStream,
  state: Arc<State>,
) -> Result<(), RequestError> {
  handshake_server(&mut stream).await?;
  let request = ctld_ipc::read_frame::<_, ClientMessage>(&mut stream)
    .await?
    .ok_or(RequestError::ClientClosed)?;
  let result = match request {
    ClientMessage::EnsureMaster { target } => ensure_master(&mut stream, state, target).await,
    ClientMessage::MasterStatus { target } => master_status(&mut stream, &state, &target).await,
    ClientMessage::ConnectionStatus { target } => {
      connection_status(&mut stream, &state, &target).await
    }
    ClientMessage::DisconnectMaster { target } => {
      disconnect_master(&mut stream, &state, &target).await
    }
    ClientMessage::DeleteCredentials { target } => delete_credentials(&mut stream, &target).await,
    ClientMessage::ConfigurePortForward {
      target,
      forward,
      enabled,
    } => configure_port_forward(&mut stream, &state, target, forward, enabled).await,
    ClientMessage::ListPortForwards { target } => {
      list_port_forwards(&mut stream, &state, &target).await
    }
    ClientMessage::ListRemoteListeners { target } => {
      list_remote_listeners(&mut stream, &state, &target).await
    }
    ClientMessage::Askpass {
      token,
      message,
      confirm,
    } => handle_askpass(&mut stream, &state, &token, message, confirm).await,
    ClientMessage::Handshake { .. } | ClientMessage::PromptResponse { .. } => {
      Err(RequestError::InvalidRequest("unexpected message"))
    }
  };
  if let Err(error) = &result {
    let _ = ctld_ipc::write_frame(
      &mut stream,
      &ServerMessage::Error {
        code: error.code().to_owned(),
        message: error.to_string(),
      },
    )
    .await;
  }
  result
}

async fn handshake(stream: &mut ctld_ipc::Stream) -> Result<(), ctld_ipc::CodecError> {
  ctld_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol_version: ctld_ipc::PROTOCOL_VERSION,
    },
  )
  .await?;
  match ctld_ipc::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION =>
    {
      Ok(())
    }
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "ctld handshake failed").into()),
  }
}

async fn handshake_server(stream: &mut ctld_ipc::Stream) -> Result<(), RequestError> {
  match ctld_ipc::read_frame::<_, ClientMessage>(stream).await? {
    Some(ClientMessage::Handshake { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION =>
    {
      ctld_ipc::write_frame(
        stream,
        &ServerMessage::HandshakeAccepted { protocol_version },
      )
      .await?;
      Ok(())
    }
    Some(ClientMessage::Handshake { protocol_version }) => {
      let error = RequestError::ProtocolVersionMismatch {
        expected: ctld_ipc::PROTOCOL_VERSION,
        actual: protocol_version,
      };
      ctld_ipc::write_frame(
        stream,
        &ServerMessage::Error {
          code: error.code().to_owned(),
          message: error.to_string(),
        },
      )
      .await?;
      Err(error)
    }
    _ => Err(RequestError::InvalidRequest("protocol handshake required")),
  }
}

async fn ensure_master(
  stream: &mut ctld_ipc::Stream,
  state: Arc<State>,
  target: SshTarget,
) -> Result<(), RequestError> {
  validate_target(&target)?;
  let lifecycle = state.target(&target);
  let mut attempt = lifecycle.attempt();
  let _target_guard = attempt.run(lifecycle.lock.lock()).await?;
  lifecycle.resume(&attempt)?;
  attempt
    .run(async { state.forwards.lock().await.resume(&target) })
    .await?;
  let mut endpoint = attempt.run(ssh_config_master::resolve(&target)).await??;
  let mut reused = attempt
    .run(reuse_master_or_prepare(&state, &target, &endpoint))
    .await??;
  if !reused && endpoint.shared {
    endpoint = endpoint.after_missing_master(&target)?;
    if !endpoint.shared {
      reused = attempt
        .run(reuse_master_or_prepare(&state, &target, &endpoint))
        .await??;
    }
  }
  let control_path = endpoint.control_path.clone();
  if reused {
    state.adopt(&target, &endpoint, None);
    attempt
      .run(async {
        state
          .forwards
          .lock()
          .await
          .activate(&SshForwardControl { state: &state }, &target)
          .await;
      })
      .await?;
    return attempt
      .run(ctld_ipc::write_frame(
        stream,
        &ServerMessage::MasterReady { control_path },
      ))
      .await?
      .map_err(Into::into);
  }
  let token = uuid::Uuid::new_v4().to_string();
  let (prompt_tx, mut prompt_rx) = mpsc::channel(1);
  state
    .attempts
    .lock()
    .unwrap()
    .insert(token.clone(), Attempt { prompts: prompt_tx });
  let _attempt_guard = AttemptGuard {
    token: token.clone(),
    state: Arc::clone(&state),
  };
  let mut child = start_master(&target, &endpoint, &token)?;
  let result = attempt
    .run(wait_for_master(
      stream,
      &state,
      &target,
      &endpoint,
      &mut child,
      &mut prompt_rx,
    ))
    .await;
  if endpoint.shared {
    if matches!(result, Ok(Ok(()))) {
      state.adopt(&target, &endpoint, child.stdin.take());
    }
    // A configured master can already serve other applications. End only our
    // anchor session; killing this process could terminate their channels.
    release_shared_process(child);
  } else if !matches!(result, Ok(Ok(()))) {
    let _ = child.kill().await;
  }
  result?
}

async fn wait_for_master(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
  endpoint: &MasterEndpoint,
  child: &mut Child,
  prompt_rx: &mut mpsc::Receiver<PromptRequest>,
) -> Result<(), RequestError> {
  let control_path = &endpoint.control_path;
  let mut authenticated = shared_session_ready(child, endpoint.shared);
  let mut diagnostics = child.stderr.take().map(|mut stderr| {
    tokio::spawn(async move {
      use tokio::io::AsyncReadExt as _;
      let mut retained = Vec::new();
      let mut buffer = [0_u8; 1024];
      while let Ok(count) = stderr.read(&mut buffer).await {
        if count == 0 {
          break;
        }
        let keep = count.min(MAX_DIAGNOSTICS.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
      }
      String::from_utf8_lossy(&retained).trim().to_owned()
    })
  });
  let deadline = Instant::now() + MASTER_START_TIMEOUT;
  let mut captured = HashMap::new();
  let mut attempted_stored = HashSet::new();
  loop {
    if control_master_is_ready(target, control_path).await {
      #[cfg(target_os = "macos")]
      handle_save_offer(stream, target, &mut captured).await?;
      #[cfg(not(target_os = "macos"))]
      captured.clear();
      state.adopt(target, endpoint, None);
      let mut forwards = state.forwards.lock().await;
      forwards
        .activate(&SshForwardControl { state }, target)
        .await;
      drop(forwards);
      ctld_ipc::write_frame(
        stream,
        &ServerMessage::MasterReady {
          control_path: control_path.clone(),
        },
      )
      .await?;
      return Ok(());
    }
    if endpoint.shared
      && authenticated
        .as_mut()
        .is_some_and(|ready| ready.try_recv().is_ok())
    {
      if control_master_is_ready(target, control_path).await {
        continue;
      }
      return Err(RequestError::SshConfig(format!(
        "OpenSSH connected without creating its configured control socket at {}. Check ControlPath and its parent directory.",
        control_path.display()
      )));
    }
    if let Some(status) = child.try_wait().map_err(RequestError::StartMaster)?
      && (!status.success() || endpoint.shared)
    {
      let message = if let Some(task) = diagnostics.take() {
        tokio::time::timeout(Duration::from_secs(1), task)
          .await
          .ok()
          .and_then(Result::ok)
          .unwrap_or_default()
      } else {
        String::new()
      };
      return Err(RequestError::MasterFailed(if message.is_empty() {
        status.to_string()
      } else {
        message
      }));
    }
    if Instant::now() >= deadline {
      return Err(RequestError::MasterTimeout);
    }
    tokio::select! {
      prompt = prompt_rx.recv() => {
        let Some(prompt) = prompt else {
          return Err(RequestError::ClientClosed);
        };
        answer_prompt(
          stream,
          target,
          prompt,
          &mut attempted_stored,
          &mut captured,
        ).await?;
      }
      () = sleep(MASTER_POLL_INTERVAL) => {}
    }
  }
}

async fn reuse_master_or_prepare(
  state: &State,
  target: &SshTarget,
  endpoint: &MasterEndpoint,
) -> Result<bool, RequestError> {
  let control_path = &endpoint.control_path;
  // The caller holds the target lock. Coordinate socket replacement with
  // listener tracking: configure may create a forward after the new master
  // is ready but before the credential save offer finishes.
  let mut forwards = state.forwards.lock().await;
  let ready = control_master_is_ready(target, control_path).await;
  if let Some(previous) = state.endpoint(target) {
    let changed = previous.control_path != *control_path || previous.shared != endpoint.shared;
    if changed || !ready {
      // Our shared listeners remain under ctld's control even when the master
      // dies. A dead private master instead needs stale-socket cleanup; asking
      // that socket to cancel a forward would prevent reconnection forever.
      if previous.shared || changed && control_master_is_ready(target, &previous.control_path).await
      {
        forwards
          .disconnect(&SshForwardControl { state }, target)
          .await?;
        forwards.resume(target);
      }
      state
        .configured_connections
        .lock()
        .unwrap()
        .remove(&target_key(target));
      forwards.master_replaced(target);
    }
  }
  if ready {
    return Ok(true);
  }
  if !endpoint.shared {
    prepare_control_path(control_path)?;
  }
  forwards.master_replaced(target);
  Ok(false)
}

impl RequestError {
  fn code(&self) -> &'static str {
    match self {
      Self::Codec(_) | Self::ClientClosed => "ctld_connection_error",
      Self::InvalidRequest(_) => "ctld_protocol_error",
      Self::ProtocolVersionMismatch { .. } => "ctld_protocol_version_mismatch",
      Self::StartMaster(_) => "ssh_start_failed",
      Self::MasterTimeout => "ssh_timeout",
      Self::MasterFailed(_) => "ssh_authentication_failed",
      Self::SshConfig(_) => "ssh_config_error",
      Self::HostDisconnected => "ssh_host_disconnected",
      Self::DisconnectFailed(_) => "ssh_disconnect_failed",
      Self::PortForwardFailed(_) => "ssh_port_forward_failed",
      Self::RemoteListenerFailed(_) => "ssh_listener_discovery_failed",
      Self::RemoteAgentUpdateRequired => "ctl_agent_update_required",
    }
  }
}

async fn answer_prompt(
  stream: &mut ctld_ipc::Stream,
  target: &SshTarget,
  prompt: PromptRequest,
  attempted_stored: &mut HashSet<String>,
  captured: &mut HashMap<String, Zeroizing<String>>,
) -> Result<(), RequestError> {
  let cacheable = !prompt.confirm && cacheable_prompt(&prompt.message);
  #[cfg(target_os = "macos")]
  if cacheable && attempted_stored.insert(prompt.message.clone()) {
    let target = target.clone();
    let message = prompt.message.clone();
    match tokio::task::spawn_blocking(move || keychain::load(&target, &message)).await {
      Ok(Ok(Some(secret))) => {
        let _ = prompt.response.send(Some(secret));
        return Ok(());
      }
      Ok(Err(error)) if !error.is_missing_entitlement() => {
        let _ = prompt.response.send(None);
        return Ok(());
      }
      Ok(Ok(None) | Err(_)) | Err(_) => {}
    }
  }
  #[cfg(not(target_os = "macos"))]
  let _ = (target, attempted_stored);

  let prompt_id = uuid::Uuid::new_v4().to_string();
  ctld_ipc::write_frame(
    stream,
    &ServerMessage::Prompt {
      prompt_id: prompt_id.clone(),
      kind: if prompt.confirm {
        PromptKind::Confirm
      } else {
        PromptKind::Secret
      },
      message: prompt.message.clone(),
    },
  )
  .await?;
  let response = match ctld_ipc::read_frame::<_, ClientMessage>(stream).await? {
    Some(ClientMessage::PromptResponse {
      prompt_id: response_id,
      response,
    }) if response_id == prompt_id => response,
    _ => return Err(RequestError::InvalidRequest("expected prompt response")),
  };
  if cacheable && let Some(secret) = &response {
    captured.insert(prompt.message, secret.clone());
  }
  let _ = prompt.response.send(response);
  Ok(())
}

#[cfg(target_os = "macos")]
async fn handle_save_offer(
  stream: &mut ctld_ipc::Stream,
  target: &SshTarget,
  captured: &mut HashMap<String, Zeroizing<String>>,
) -> Result<(), RequestError> {
  if captured.is_empty() {
    return Ok(());
  }
  let policy_target = target.clone();
  let should_offer =
    tokio::task::spawn_blocking(move || keychain::should_offer_save(&policy_target))
      .await
      .map_err(|_| RequestError::InvalidRequest("keychain worker stopped"));
  let should_offer = match should_offer {
    Ok(Ok(value)) => value,
    Ok(Err(error)) => {
      captured.clear();
      report_save_error(stream, &error.to_string()).await?;
      return Ok(());
    }
    Err(error) => return Err(error),
  };
  if !should_offer {
    captured.clear();
    return Ok(());
  }
  let response = request_ui(
    stream,
    PromptKind::CredentialSave,
    "Save this SSH password or key passphrase for future connections? It will be stored device-locally in Keychain and require Touch ID.",
  )
  .await?;
  match response.as_deref().map(String::as_str) {
    Some("yes") => {
      let target = target.clone();
      let secrets = std::mem::take(captured);
      if let Err(error) = tokio::task::spawn_blocking(move || keychain::save(&target, &secrets))
        .await
        .map_err(|_| RequestError::InvalidRequest("keychain worker stopped"))?
      {
        report_save_error(stream, &error.to_string()).await?;
      }
    }
    Some("never") => {
      captured.clear();
      let target = target.clone();
      if let Err(error) = tokio::task::spawn_blocking(move || keychain::never_save(&target))
        .await
        .map_err(|_| RequestError::InvalidRequest("keychain worker stopped"))?
      {
        report_save_error(stream, &error.to_string()).await?;
      }
    }
    _ => captured.clear(),
  }
  Ok(())
}

#[cfg(target_os = "macos")]
async fn report_save_error(
  stream: &mut ctld_ipc::Stream,
  message: &str,
) -> Result<(), RequestError> {
  let message = if message.contains("-34018") {
    "ctld must be signed with its application identifier entitlement before it can use the Touch ID-protected Keychain.".to_owned()
  } else {
    format!("Could not update the SSH credential in Keychain: {message}")
  };
  let _ = request_ui(
    stream,
    PromptKind::CredentialSaveError,
    &format!("Connected, but the credential was not saved. {message}"),
  )
  .await?;
  Ok(())
}

#[cfg(target_os = "macos")]
async fn request_ui(
  stream: &mut ctld_ipc::Stream,
  kind: PromptKind,
  message: &str,
) -> Result<Option<Zeroizing<String>>, RequestError> {
  let prompt_id = uuid::Uuid::new_v4().to_string();
  ctld_ipc::write_frame(
    stream,
    &ServerMessage::Prompt {
      prompt_id: prompt_id.clone(),
      kind,
      message: message.to_owned(),
    },
  )
  .await?;
  match ctld_ipc::read_frame::<_, ClientMessage>(stream).await? {
    Some(ClientMessage::PromptResponse {
      prompt_id: response_id,
      response,
    }) if response_id == prompt_id => Ok(response),
    _ => Err(RequestError::InvalidRequest("expected prompt response")),
  }
}

async fn handle_askpass(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  token: &str,
  message: String,
  confirm: bool,
) -> Result<(), RequestError> {
  let prompts = state
    .attempts
    .lock()
    .unwrap()
    .get(token)
    .map(|attempt| attempt.prompts.clone())
    .ok_or(RequestError::InvalidRequest("unknown askpass attempt"))?;
  let (response_tx, response_rx) = oneshot::channel();
  prompts
    .send(PromptRequest {
      message,
      confirm,
      response: response_tx,
    })
    .await
    .map_err(|_| RequestError::ClientClosed)?;
  let response = response_rx.await.map_err(|_| RequestError::ClientClosed)?;
  ctld_ipc::write_frame(stream, &ServerMessage::AskpassResponse { response }).await?;
  Ok(())
}

async fn master_status(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let lifecycle = state.target(target);
  let mut attempt = lifecycle.attempt();
  lifecycle.require_connected()?;
  let message = if let Some(endpoint) = state.endpoint(target) {
    if attempt
      .run(control_master_is_ready(target, &endpoint.control_path))
      .await?
    {
      ServerMessage::MasterReady {
        control_path: endpoint.control_path,
      }
    } else {
      ServerMessage::AuthenticationRequired
    }
  } else {
    ServerMessage::AuthenticationRequired
  };
  ctld_ipc::write_frame(stream, &message).await?;
  Ok(())
}

async fn disconnect_master(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let lifecycle = state.target(target);
  // Publish cancellation before waiting for the lock held by authentication,
  // including an unanswered password or credential-save prompt.
  lifecycle.pause();
  let _target_guard = lifecycle.lock.lock().await;
  let mut forwards = state.forwards.lock().await;
  forwards.pause(target);
  if let Some(endpoint) = state.endpoint(target) {
    if endpoint.shared {
      forwards
        .disconnect(&SshForwardControl { state }, target)
        .await?;
    } else {
      exit_master(target, &endpoint.control_path).await?;
    }
  }
  state
    .configured_connections
    .lock()
    .unwrap()
    .remove(&target_key(target));
  forwards.master_replaced(target);
  ctld_ipc::write_frame(stream, &ServerMessage::MasterDisconnected).await?;
  Ok(())
}

async fn connection_status(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let connected = if state.target(target).is_paused() {
    false
  } else if let Some(endpoint) = state.endpoint(target) {
    control_master_is_ready(target, &endpoint.control_path).await
  } else {
    false
  };
  ctld_ipc::write_frame(
    stream,
    &ServerMessage::ConnectionStatus {
      connected,
      manually_disconnected: state.target(target).is_paused(),
    },
  )
  .await?;
  Ok(())
}

fn exit_master_command(target: &SshTarget, control_path: &Path) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command.arg("-S").arg(control_path).args(["-O", "exit"]);
  append_target_arguments(&mut command, target);
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  command
}

async fn exit_master(target: &SshTarget, control_path: &Path) -> Result<(), RequestError> {
  if !control_path.exists() {
    return Ok(());
  }
  let output = tokio::time::timeout(
    MASTER_CHECK_TIMEOUT,
    exit_master_command(target, control_path).output(),
  )
  .await
  .map_err(|_| RequestError::DisconnectFailed("control command timed out".into()))?
  .map_err(|error| RequestError::DisconnectFailed(error.to_string()))?;
  if output.status.success() || !control_path.exists() {
    return Ok(());
  }
  let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
  Err(RequestError::DisconnectFailed(if message.is_empty() {
    output.status.to_string()
  } else {
    message
  }))
}

async fn delete_credentials(
  stream: &mut ctld_ipc::Stream,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  #[cfg(target_os = "macos")]
  {
    let target = target.clone();
    tokio::task::spawn_blocking(move || keychain::delete(&target))
      .await
      .map_err(|_| RequestError::InvalidRequest("keychain worker stopped"))?
      .map_err(|_| RequestError::InvalidRequest("could not delete Keychain credential"))?;
  }
  ctld_ipc::write_frame(stream, &ServerMessage::CredentialsDeleted).await?;
  Ok(())
}

async fn configure_port_forward(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: SshTarget,
  forward: LocalPortForward,
  enabled: bool,
) -> Result<(), RequestError> {
  validate_target(&target)?;
  validate_forward(&forward)?;
  // No target lock here: ensure_master holds its target lock before activating
  // forwards. The registry lock serializes all listener ownership changes.
  let mut forwards = state.forwards.lock().await;
  if state.target(&target).is_paused() {
    forwards.pause(&target);
  }
  let status = forwards
    .configure(&SshForwardControl { state }, target, forward, enabled)
    .await?;
  ctld_ipc::write_frame(stream, &ServerMessage::PortForwardConfigured { status }).await?;
  Ok(())
}

async fn list_port_forwards(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let statuses = state
    .forwards
    .lock()
    .await
    .list(&SshForwardControl { state }, target)
    .await;
  ctld_ipc::write_frame(stream, &ServerMessage::PortForwards { statuses }).await?;
  Ok(())
}

async fn list_remote_listeners(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let lifecycle = state.target(target);
  let mut attempt = lifecycle.attempt();
  lifecycle.require_connected()?;
  let _target_guard = attempt.run(lifecycle.lock.lock()).await?;
  lifecycle.require_connected()?;
  let Some(endpoint) = state.endpoint(target) else {
    ctld_ipc::write_frame(stream, &ServerMessage::AuthenticationRequired).await?;
    return Ok(());
  };
  let control_path = endpoint.control_path;
  if !attempt
    .run(control_master_is_ready(target, &control_path))
    .await?
  {
    ctld_ipc::write_frame(stream, &ServerMessage::AuthenticationRequired).await?;
    return Ok(());
  }

  let catalog = attempt
    .run(run_listener_discovery(target, &control_path))
    .await??;
  ctld_ipc::write_frame(stream, &ServerMessage::RemoteListeners { catalog }).await?;
  Ok(())
}

fn listener_discovery_command(target: &SshTarget, control_path: &Path) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  command
    .arg("-S")
    .arg(control_path)
    .arg("-T")
    .args(["-o", "ControlMaster=no"])
    // The master can disappear after its readiness check. Never fall back to
    // a new direct or gateway connection during this noninteractive request.
    .args(["-o", "ProxyCommand=false"])
    .args(["-o", "ClearAllForwardings=yes"])
    .args(["-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no"])
    .args(["-o", "PermitLocalCommand=no"])
    .args(["-o", "RemoteCommand=none"])
    .args(["-o", "BatchMode=yes"])
    .args(["-o", "ForkAfterAuthentication=no"]);
  append_target_arguments(&mut command, target);
  command
    .arg(REMOTE_LISTENERS_COMMAND)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  command
}

async fn run_listener_discovery(
  target: &SshTarget,
  control_path: &Path,
) -> Result<ctl_proto::TcpListenerCatalog, RequestError> {
  let mut child = listener_discovery_command(target, control_path)
    .spawn()
    .map_err(RequestError::StartMaster)?;
  let mut stdout = child.stdout.take().ok_or(RequestError::InvalidRequest(
    "listener stdout was not piped",
  ))?;
  let mut stderr = child.stderr.take().ok_or(RequestError::InvalidRequest(
    "listener stderr was not piped",
  ))?;
  let output =
    tokio::spawn(async move { read_bounded(&mut stdout, MAX_LISTENER_CATALOG_BYTES).await });
  let diagnostics = tokio::spawn(async move { read_bounded(&mut stderr, MAX_DIAGNOSTICS).await });
  let status =
    if let Ok(result) = tokio::time::timeout(LISTENER_DISCOVERY_TIMEOUT, child.wait()).await {
      result.map_err(RequestError::StartMaster)?
    } else {
      let _ = child.kill().await;
      return Err(RequestError::RemoteListenerFailed(
        "request timed out".into(),
      ));
    };
  let (output, output_overflowed) = output
    .await
    .map_err(|_| RequestError::InvalidRequest("listener output worker stopped"))?
    .map_err(RequestError::StartMaster)?;
  let (diagnostics, _) = diagnostics
    .await
    .map_err(|_| RequestError::InvalidRequest("listener diagnostics worker stopped"))?
    .map_err(RequestError::StartMaster)?;
  let diagnostics = String::from_utf8_lossy(&diagnostics).trim().to_owned();
  if output_overflowed {
    return Err(RequestError::RemoteListenerFailed(
      "ctl-agent returned too much listener data".into(),
    ));
  }
  if !status.success() {
    if listener_discovery_requires_agent_update(&diagnostics) {
      return Err(RequestError::RemoteAgentUpdateRequired);
    }
    return Err(RequestError::RemoteListenerFailed(
      if diagnostics.is_empty() {
        status.to_string()
      } else {
        diagnostics
      },
    ));
  }
  let catalog: ctl_proto::TcpListenerCatalog =
    serde_json::from_slice(&output).map_err(|error| {
      RequestError::RemoteListenerFailed(format!("invalid ctl-agent output: {error}"))
    })?;
  validate_listener_catalog(&catalog)?;
  Ok(catalog)
}

fn listener_discovery_requires_agent_update(diagnostics: &str) -> bool {
  let diagnostics = diagnostics.to_ascii_lowercase();
  diagnostics == "ctl-agent is not installed"
    || diagnostics.contains("usage: ctl-agent")
      && [
        "unrecognized subcommand 'listeners'",
        "unrecognized subcommand `listeners`",
        "unexpected argument 'listeners'",
        "unexpected argument `listeners`",
      ]
      .iter()
      .any(|message| diagnostics.contains(message))
}

async fn read_bounded(
  reader: &mut (impl tokio::io::AsyncRead + Unpin),
  maximum: usize,
) -> io::Result<(Vec<u8>, bool)> {
  use tokio::io::AsyncReadExt as _;

  let mut retained = Vec::new();
  let mut overflowed = false;
  let mut buffer = [0_u8; 4096];
  loop {
    let count = reader.read(&mut buffer).await?;
    if count == 0 {
      return Ok((retained, overflowed));
    }
    let keep = count.min(maximum.saturating_sub(retained.len()));
    retained.extend_from_slice(&buffer[..keep]);
    overflowed |= keep < count;
  }
}

fn validate_listener_catalog(catalog: &ctl_proto::TcpListenerCatalog) -> Result<(), RequestError> {
  if catalog.listeners.len() > 4096
    || catalog.warnings.len() > 32
    || catalog.listeners.iter().any(|listener| {
      listener.port == 0 || listener.bind_address.parse::<std::net::IpAddr>().is_err()
    })
    || catalog.warnings.iter().any(|warning| {
      warning.is_empty() || warning.len() > 1024 || warning.chars().any(char::is_control)
    })
  {
    return Err(RequestError::RemoteListenerFailed(
      "ctl-agent returned an invalid listener catalog".into(),
    ));
  }
  Ok(())
}

async fn run_forward_command(
  target: &SshTarget,
  control_path: &Path,
  forward: &LocalPortForward,
  cancel: bool,
) -> Result<(), RequestError> {
  let specification = format!(
    "{}:{}:{}:{}",
    forward.bind_address,
    forward.local_port,
    remote_forward_host(&forward.remote_host),
    forward.remote_port
  );
  let mut command = Command::new(SSH_PROGRAM);
  command
    .arg("-S")
    .arg(control_path)
    .args(["-O", if cancel { "cancel" } else { "forward" }])
    .arg("-L")
    .arg(specification);
  append_target_arguments(&mut command, target);
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let output = tokio::time::timeout(MASTER_CHECK_TIMEOUT, command.output())
    .await
    .map_err(|_| RequestError::PortForwardFailed("control command timed out".into()))?
    .map_err(RequestError::StartMaster)?;
  if output.status.success() {
    return Ok(());
  }
  let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
  Err(RequestError::PortForwardFailed(if message.is_empty() {
    output.status.to_string()
  } else {
    message
  }))
}

fn remote_forward_host(host: &str) -> String {
  if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
    format!("[{host}]")
  } else {
    host.to_owned()
  }
}

fn master_command(target: &SshTarget, endpoint: &MasterEndpoint) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  if endpoint.shared {
    ssh_config_master::append_session_options(&mut command);
  } else {
    command
      .args(["-f", "-M", "-N", "-T"])
      .args(["-o", "ControlMaster=yes"])
      .args(["-o", &format!("ControlPersist={MASTER_IDLE_SECONDS}")])
      .arg("-S")
      .arg(&endpoint.control_path)
      .args(["-o", "ClearAllForwardings=yes"])
      .args(["-o", "ForwardAgent=no"])
      .args(["-o", "ForwardX11=no"])
      .args(["-o", "PermitLocalCommand=no"])
      .args(["-o", "RemoteCommand=none"])
      .args(["-o", "BatchMode=no"])
      .args(["-o", "StrictHostKeyChecking=ask"]);
  }
  append_target_arguments(&mut command, target);
  if endpoint.shared {
    command.arg(ssh_config_master::SHARED_SESSION_COMMAND);
  }
  command
}

fn start_master(
  target: &SshTarget,
  endpoint: &MasterEndpoint,
  token: &str,
) -> Result<Child, RequestError> {
  if endpoint.shared && endpoint.startup != SharedMasterStartup::Create {
    return Err(RequestError::SshConfig(
      "The configured SSH master disappeared before it could be reused. Connect again to reevaluate its configuration.".into(),
    ));
  }
  let current_executable = std::env::current_exe().map_err(RequestError::StartMaster)?;
  let mut command = master_command(target, endpoint);
  command
    .env("SSH_ASKPASS", current_executable)
    .env("SSH_ASKPASS_REQUIRE", "force")
    .env("DISPLAY", "ctld-askpass")
    .env("CTLD_ASKPASS", "1")
    .env("CTLD_ASKPASS_TOKEN", token)
    .stdin(if endpoint.shared {
      Stdio::piped()
    } else {
      Stdio::null()
    })
    .stdout(if endpoint.shared {
      Stdio::piped()
    } else {
      Stdio::null()
    })
    .stderr(Stdio::piped())
    .kill_on_drop(!endpoint.shared)
    .spawn()
    .map_err(RequestError::StartMaster)
}

fn release_shared_process(mut child: Child) {
  drop(child.stdin.take());
  // Socket publication can race with cancellation. Even a missing path cannot
  // prove that killing this process is safe for other clients. Close only our
  // session and let OpenSSH apply its configured lifetime and network timeouts.
  tokio::spawn(async move {
    let _ = child.wait().await;
  });
}

fn shared_session_ready(child: &mut Child, shared: bool) -> Option<oneshot::Receiver<()>> {
  if !shared {
    return None;
  }
  let stdout = child.stdout.take()?;
  let (ready, receiver) = oneshot::channel();
  tokio::spawn(async move {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, BufReader};
    let mut lines = BufReader::new(stdout.take(MAX_DIAGNOSTICS as u64)).lines();
    while let Ok(Some(line)) = lines.next_line().await {
      if line == "ctld-master-ready" {
        let _ = ready.send(());
        return;
      }
    }
  });
  Some(receiver)
}

fn prepare_control_path(control_path: &Path) -> Result<(), RequestError> {
  if let Some(parent) = control_path.parent() {
    #[cfg(unix)]
    {
      let base = parent.parent().ok_or(RequestError::InvalidRequest(
        "control path has no private base",
      ))?;
      prepare_private_directory(base).map_err(RequestError::StartMaster)?;
      prepare_private_directory(parent).map_err(RequestError::StartMaster)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(parent).map_err(RequestError::StartMaster)?;
  }
  if control_path.exists() {
    std::fs::remove_file(control_path).map_err(RequestError::StartMaster)?;
  }
  Ok(())
}

async fn control_master_is_ready(target: &SshTarget, path: &Path) -> bool {
  if !path.exists() {
    return false;
  }
  let mut command = Command::new(SSH_PROGRAM);
  command.arg("-S").arg(path).args(["-O", "check"]);
  append_target_arguments(&mut command, target);
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true);
  tokio::time::timeout(MASTER_CHECK_TIMEOUT, command.status())
    .await
    .is_ok_and(|result| result.is_ok_and(|status| status.success()))
}

fn append_target_arguments(command: &mut Command, target: &SshTarget) {
  if !target.gateways.is_empty() {
    // -o respects a prior fail-closed ProxyCommand; -J rejects that combination.
    command.arg("-o").arg(format!(
      "ProxyJump={}",
      target
        .gateways
        .iter()
        .map(gateway_jump_specification)
        .collect::<Vec<_>>()
        .join(","),
    ));
  }
  if let Some(port) = target.port {
    command.args(["-p", &port.to_string()]);
  }
  if let Some(user) = &target.user {
    command.args(["-l", user]);
  }
  if let Some(identity_file) = &target.identity_file {
    command.arg("-i").arg(identity_file);
  }
  if let Some(alias) = &target.ssh_config_alias {
    if let Some(hostname) = &target.hostname {
      command.arg("-o").arg(format!("HostName={hostname}"));
    }
    command.arg("--").arg(alias);
  } else {
    command
      .arg("--")
      .arg(target.hostname.as_deref().unwrap_or(&target.destination));
  }
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

fn control_path(target: &SshTarget) -> PathBuf {
  control_path_for_socket(target, &ctld_ipc::socket_path())
}

fn control_path_for_socket(target: &SshTarget, daemon_socket: &Path) -> PathBuf {
  // OpenSSH adds a random suffix while binding a control socket. Keep the
  // entire path independent of potentially long runtime-directory paths.
  // Including the daemon endpoint prevents normal and development brokers
  // from sharing a master for the same destination.
  let mut hasher = Sha256::new();
  hasher.update(daemon_socket.to_string_lossy().as_bytes());
  hasher.update([0]);
  hasher.update(target_key(target).as_bytes());
  let digest = hasher.finalize();
  let mut name = String::with_capacity(32);
  for byte in &digest[..16] {
    write!(name, "{byte:02x}").expect("writing to a String cannot fail");
  }
  #[cfg(unix)]
  let directory = PathBuf::from("/tmp")
    .join(format!("ctld-{}", rustix::process::getuid().as_raw()))
    .join("masters");
  #[cfg(not(unix))]
  let directory = std::env::temp_dir().join("ctld-masters");
  directory.join(name)
}

fn validate_target(target: &SshTarget) -> Result<(), RequestError> {
  if target.ssh_config_alias.as_ref().is_some_and(|alias| {
    alias != &target.destination
      || alias.starts_with(['-', '!'])
      || alias
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '*' | '?'))
  }) || target.destination.trim().is_empty()
    || target.destination.chars().any(char::is_control)
    || target.hostname.as_ref().is_some_and(|value| {
      value.trim().is_empty()
        || value
          .chars()
          .any(|character| character.is_control() || character.is_whitespace())
    })
    || target.user.as_ref().is_some_and(|value| {
      value.trim().is_empty()
        || value
          .chars()
          .any(|character| character.is_control() || character.is_whitespace())
    })
    || target.port == Some(0)
    || target
      .identity_file
      .as_ref()
      .is_some_and(|path| path.as_os_str().is_empty())
  {
    return Err(RequestError::InvalidRequest("invalid SSH target"));
  }
  if target.gateways.len() > 8 || target.gateways.iter().any(invalid_gateway) {
    return Err(RequestError::InvalidRequest("invalid SSH gateway route"));
  }
  Ok(())
}

fn invalid_gateway(gateway: &SshGateway) -> bool {
  gateway.destination.trim().is_empty()
    || gateway.destination.chars().any(char::is_control)
    || gateway
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
    || gateway.identity_file.is_some()
    || gateway.mode == SshGatewayMode::AgentRelayOnly
}

fn target_key(target: &SshTarget) -> String {
  let bytes = serde_json::to_vec(target).expect("SSH targets are always serializable");
  Sha256::digest(bytes)
    .iter()
    .fold(String::with_capacity(64), |mut encoded, byte| {
      write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
      encoded
    })
}

fn validate_forward(forward: &LocalPortForward) -> Result<(), RequestError> {
  let valid_host = |value: &str| {
    !value.trim().is_empty()
      && value.len() <= 255
      && !value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
  };
  if forward.forward_id.is_empty()
    || forward.forward_id.len() > 128
    || forward.forward_id.chars().any(char::is_control)
    || !matches!(forward.bind_address.as_str(), "127.0.0.1" | "::1")
    || forward.local_port == 0
    || forward.remote_port == 0
    || !valid_host(&forward.remote_host)
  {
    return Err(RequestError::InvalidRequest("invalid local port forward"));
  }
  Ok(())
}

fn cacheable_prompt(message: &str) -> bool {
  let lower = message.to_lowercase();
  lower.contains("password:") || lower.starts_with("enter passphrase for key")
}

fn is_host_confirmation(message: &str) -> bool {
  let message = message.trim_end();
  message.ends_with("Are you sure you want to continue connecting (yes/no/[fingerprint])?")
    || message.ends_with("Are you sure you want to continue connecting (yes/no)?")
}

#[cfg(unix)]
fn prepare_runtime_directory(socket_path: &Path) -> io::Result<()> {
  let directory = socket_path
    .parent()
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "ctld socket has no parent"))?;
  prepare_private_directory(directory)
}

#[cfg(unix)]
fn prepare_private_directory(directory: &Path) -> io::Result<()> {
  let existed = directory.exists();
  std::fs::create_dir_all(directory)?;
  let mut metadata = std::fs::symlink_metadata(directory)?;
  if metadata.file_type().is_symlink()
    || !metadata.is_dir()
    || metadata.uid() != rustix::process::getuid().as_raw()
  {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "ctld runtime directory is not an owner-controlled directory",
    ));
  }
  if !existed {
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    metadata = std::fs::symlink_metadata(directory)?;
  }
  if metadata.permissions().mode() & 0o077 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      "ctld runtime directory is not private and owner-only",
    ));
  }
  Ok(())
}

#[cfg(unix)]
async fn bind_listener(path: &Path) -> Result<UnixListener, DaemonError> {
  let listener = match UnixListener::bind(path) {
    Ok(listener) => listener,
    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
      if tokio::net::UnixStream::connect(path).await.is_ok() {
        return Err(DaemonError::Bind {
          path: path.to_path_buf(),
          source: io::Error::new(io::ErrorKind::AddrInUse, "ctld is already running"),
        });
      }
      std::fs::remove_file(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })?;
      UnixListener::bind(path).map_err(|source| DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      })?
    }
    Err(source) => {
      return Err(DaemonError::Bind {
        path: path.to_path_buf(),
        source,
      });
    }
  };
  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
    DaemonError::Bind {
      path: path.to_path_buf(),
      source,
    }
  })?;
  let metadata = std::fs::symlink_metadata(path).map_err(|source| DaemonError::Bind {
    path: path.to_path_buf(),
    source,
  })?;
  if !metadata.file_type().is_socket()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.permissions().mode() & 0o077 != 0
  {
    return Err(DaemonError::Bind {
      path: path.to_path_buf(),
      source: io::Error::new(io::ErrorKind::PermissionDenied, "insecure ctld endpoint"),
    });
  }
  Ok(listener)
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(unix)]
  #[tokio::test]
  async fn handshake_accepts_the_current_protocol() {
    let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
    let server = tokio::spawn(async move { handshake_server(&mut server).await });
    ctld_ipc::write_frame(
      &mut client,
      &ClientMessage::Handshake {
        protocol_version: ctld_ipc::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut client).await.unwrap(),
      Some(ServerMessage::HandshakeAccepted { protocol_version })
        if protocol_version == ctld_ipc::PROTOCOL_VERSION
    ));
    server.await.unwrap().unwrap();
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn handshake_rejection_reports_both_protocol_versions() {
    let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
    let server = tokio::spawn(async move { handshake_server(&mut server).await });
    let old_version = ctld_ipc::PROTOCOL_VERSION - 1;
    ctld_ipc::write_frame(
      &mut client,
      &ClientMessage::Handshake {
        protocol_version: old_version,
      },
    )
    .await
    .unwrap();
    let Some(ServerMessage::Error { code, message }) =
      ctld_ipc::read_frame(&mut client).await.unwrap()
    else {
      panic!("expected a structured handshake rejection");
    };
    assert_eq!(code, "ctld_protocol_version_mismatch");
    assert!(message.contains(&format!(
      "requires local protocol {}",
      ctld_ipc::PROTOCOL_VERSION
    )));
    assert!(message.contains(&format!("client requested {old_version}")));
    assert!(matches!(
      server.await.unwrap(),
      Err(RequestError::ProtocolVersionMismatch { expected, actual })
        if expected == ctld_ipc::PROTOCOL_VERSION && actual == old_version
    ));
  }

  fn target() -> SshTarget {
    SshTarget {
      ssh_config_alias: None,
      destination: "work".into(),
      hostname: Some("example.test".into()),
      user: Some("alice".into()),
      port: Some(2222),
      identity_file: Some(PathBuf::from("/keys/work key")),
      gateways: Vec::new(),
    }
  }

  #[test]
  fn disconnect_exits_only_the_requested_control_master() {
    let path = Path::new("/private/tmp/ctld-test/master.sock");
    let command = exit_master_command(&target(), path);
    let args: Vec<_> = command.as_std().get_args().collect();
    assert_eq!(&args[..4], ["-S", path.to_str().unwrap(), "-O", "exit"]);
    assert_eq!(*args.last().unwrap(), "example.test");
    assert_eq!(command.as_std().get_program(), "ssh");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn listener_discovery_missing_master_never_contacts_the_host_or_gateway() {
    for through_gateway in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let port = listener.local_addr().unwrap().port();
      let target = SshTarget {
        hostname: Some("127.0.0.1".into()),
        port: Some(port),
        identity_file: None,
        gateways: if through_gateway {
          vec![SshGateway {
            destination: "127.0.0.1".into(),
            hostname: None,
            user: None,
            port: Some(port),
            identity_file: None,
            mode: SshGatewayMode::Automatic,
          }]
        } else {
          Vec::new()
        },
        ..target()
      };
      let path = PathBuf::from(format!("/tmp/ctld-mux-{}", uuid::Uuid::new_v4().simple()));
      assert!(!path.exists());
      let production = listener_discovery_command(&target, &path);
      let mut command = Command::new(SSH_PROGRAM);
      command
        .args(["-F", "/dev/null", "-o", "ConnectTimeout=1"])
        .args(production.as_std().get_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
      let output = tokio::select! {
        biased;
        accepted = listener.accept() => {
          drop(accepted);
          panic!("listener discovery attempted fresh SSH (gateway={through_gateway})");
        }
        output = tokio::time::timeout(Duration::from_secs(5), command.output()) => output.unwrap().unwrap(),
      };
      assert!(!output.status.success());
      let diagnostics = String::from_utf8_lossy(&output.stderr);
      assert!(!diagnostics.contains("Cannot specify -J with ProxyCommand"));
      assert!(diagnostics.contains("Connection closed"), "{diagnostics}");
    }
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn disconnect_is_idempotent_and_noninteractive_work_stays_paused() {
    let state = State::default();
    let target = SshTarget {
      destination: uuid::Uuid::new_v4().to_string(),
      ..target()
    };
    assert!(!control_path(&target).exists());
    let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
    for _ in 0..2 {
      disconnect_master(&mut server, &state, &target)
        .await
        .unwrap();
      assert!(matches!(
        ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
          .await
          .unwrap(),
        Some(ServerMessage::MasterDisconnected)
      ));
    }
    assert!(matches!(
      master_status(&mut server, &state, &target).await,
      Err(RequestError::HostDisconnected)
    ));
    assert!(matches!(
      list_remote_listeners(&mut server, &state, &target).await,
      Err(RequestError::HostDisconnected)
    ));
    connection_status(&mut server, &state, &target)
      .await
      .unwrap();
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap(),
      Some(ServerMessage::ConnectionStatus {
        connected: false,
        manually_disconnected: true
      })
    ));
    let other = SshTarget {
      destination: "another-host".into(),
      ..target.clone()
    };
    assert!(!state.target(&other).is_paused());
    let lifecycle = state.target(&target);
    lifecycle.resume(&lifecycle.attempt()).unwrap();
    master_status(&mut server, &state, &target).await.unwrap();
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap(),
      Some(ServerMessage::AuthenticationRequired)
    ));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn disconnect_interrupts_an_unanswered_ssh_prompt_and_releases_the_target_lock() {
    let lifecycle = Arc::new(TargetLifecycle::default());
    let worker_lifecycle = Arc::clone(&lifecycle);
    let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
    let (response, response_rx) = oneshot::channel();
    let worker = tokio::spawn(async move {
      let mut attempt = worker_lifecycle.attempt();
      let _guard = worker_lifecycle.lock.lock().await;
      attempt
        .run(answer_prompt(
          &mut server,
          &target(),
          PromptRequest {
            message: "Trust host?".into(),
            confirm: true,
            response,
          },
          &mut HashSet::new(),
          &mut HashMap::new(),
        ))
        .await
    });
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap(),
      Some(ServerMessage::Prompt { .. })
    ));
    lifecycle.pause();
    assert!(matches!(
      tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap(),
      Err(RequestError::HostDisconnected)
    ));
    assert!(response_rx.await.is_err());
    assert!(lifecycle.lock.try_lock().is_ok());
  }

  #[test]
  fn only_passwords_and_private_key_passphrases_are_cacheable() {
    assert!(cacheable_prompt("alice@example.test's password:"));
    assert!(cacheable_prompt("Enter passphrase for key '/keys/work':"));
    assert!(!cacheable_prompt("Verification code:"));
    assert!(!cacheable_prompt(
      "Are you sure you want to continue connecting (yes/no/[fingerprint])?"
    ));
  }

  #[test]
  fn host_confirmation_detection_is_narrow() {
    assert!(is_host_confirmation(
      "Are you sure you want to continue connecting (yes/no/[fingerprint])?"
    ));
    assert!(is_host_confirmation(
      "Are you sure you want to continue connecting (yes/no)?"
    ));
    assert!(!is_host_confirmation("Continue connecting?"));
  }

  #[test]
  fn control_paths_are_stable_short_and_target_specific() {
    let daemon_socket = Path::new(
      "/var/folders/very/long/runtime/directory/that/must/not/affect/control/paths/ctld.sock",
    );
    let first = control_path_for_socket(&target(), daemon_socket);
    let second = control_path_for_socket(&target(), daemon_socket);
    let mut other = target();
    other.destination = "personal".into();
    let other = control_path_for_socket(&other, daemon_socket);

    assert_eq!(first, second);
    assert_ne!(first, other);
    let mut same_alias = target();
    same_alias.hostname = None;
    same_alias.user = None;
    same_alias.port = None;
    same_alias.identity_file = None;
    assert_ne!(first, control_path_for_socket(&same_alias, daemon_socket));
    let mut routed = target();
    routed.gateways.push(SshGateway {
      destination: "edge.example".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    });
    assert_ne!(first, control_path_for_socket(&routed, daemon_socket));
    assert_ne!(
      first,
      control_path_for_socket(&target(), Path::new("/tmp/another-ctld.sock"))
    );
    assert_eq!(first.file_name().unwrap().len(), 32);
    assert!(first.as_os_str().len() < 80);
  }

  #[test]
  fn unsafe_or_ambiguous_targets_are_rejected() {
    for invalid in [
      SshTarget {
        destination: String::new(),
        ..target()
      },
      SshTarget {
        hostname: Some("host with spaces".into()),
        ..target()
      },
      SshTarget {
        user: Some("bad\nuser".into()),
        ..target()
      },
      SshTarget {
        port: Some(0),
        ..target()
      },
      SshTarget {
        identity_file: Some(PathBuf::new()),
        ..target()
      },
    ] {
      assert!(validate_target(&invalid).is_err());
    }
  }

  #[test]
  fn old_listener_commands_require_an_agent_update_without_masking_other_failures() {
    assert!(listener_discovery_requires_agent_update(
      "error: unrecognized subcommand 'listeners'\n\nUsage: ctl-agent <COMMAND>"
    ));
    assert!(listener_discovery_requires_agent_update(
      "ctl-agent is not installed"
    ));
    assert!(!listener_discovery_requires_agent_update(
      "Permission denied (publickey)."
    ));
    assert!(!listener_discovery_requires_agent_update(
      "error: unrecognized subcommand 'connect'\nUsage: ctl-agent <COMMAND>"
    ));
  }

  #[test]
  fn local_forwards_are_structured_and_loopback_only() {
    let valid = LocalPortForward {
      forward_id: "database".into(),
      bind_address: "127.0.0.1".into(),
      local_port: 15432,
      remote_host: "database.internal".into(),
      remote_port: 5432,
    };
    assert!(validate_forward(&valid).is_ok());
    assert!(
      validate_forward(&LocalPortForward {
        bind_address: "0.0.0.0".into(),
        ..valid.clone()
      })
      .is_err()
    );
    assert!(
      validate_forward(&LocalPortForward {
        remote_host: "host name".into(),
        ..valid
      })
      .is_err()
    );
    assert_eq!(remote_forward_host("127.0.0.1"), "127.0.0.1");
    assert_eq!(remote_forward_host("::1"), "[::1]");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn owner_endpoint_handshakes_and_returns_structured_request_errors() {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let directory = PathBuf::from("/tmp").join(format!("ctld-test-{}", &id[..8]));
    let socket = directory.join("ctld.sock");
    let daemon_socket = socket.clone();
    let mut daemon = tokio::spawn(async move { run(daemon_socket).await });
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut stream = loop {
      let connected = tokio::select! {
        result = tokio::net::UnixStream::connect(&socket) => result,
        result = &mut daemon => match result {
          Ok(Err(DaemonError::Bind { source, .. }))
            if source.kind() == io::ErrorKind::PermissionDenied =>
          {
            // Some test sandboxes prohibit Unix socket creation entirely.
            let _ = std::fs::remove_dir(&directory);
            return;
          }
          result => panic!("ctld stopped during startup: {result:?}"),
        },
      };
      match connected {
        Ok(stream) => break stream,
        Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(10)).await,
        Err(error) => panic!("ctld did not start: {error}"),
      }
    };
    assert_eq!(
      std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
      0o600
    );
    ctld_ipc::write_frame(
      &mut stream,
      &ClientMessage::Handshake {
        protocol_version: ctld_ipc::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
        .await
        .unwrap(),
      Some(ServerMessage::HandshakeAccepted { .. })
    ));
    let mut invalid = target();
    invalid.port = Some(0);
    ctld_ipc::write_frame(
      &mut stream,
      &ClientMessage::MasterStatus { target: invalid },
    )
    .await
    .unwrap();
    assert!(matches!(
      ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
        .await
        .unwrap(),
      Some(ServerMessage::Error { code, .. }) if code == "ctld_protocol_error"
    ));

    daemon.abort();
    let _ = daemon.await;
    assert!(!socket.exists());
    std::fs::remove_dir(directory).unwrap();
  }
}
