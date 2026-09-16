//! Per-user owner of authenticated OpenSSH control masters and SSH credentials.

#[cfg(target_os = "macos")]
mod keychain;

use ctld_ipc::{
  ClientMessage, LocalPortForward, PortForwardState, PortForwardStatus, PromptKind, ServerMessage,
  SshTarget,
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
use tokio::process::{Child, Command};
use tokio::sync::{Mutex as AsyncMutex, mpsc, oneshot};
use tokio::time::{Instant, sleep};
use zeroize::Zeroizing;

const SSH_PROGRAM: &str = "ssh";
const MASTER_IDLE_SECONDS: u64 = 300;
const MASTER_START_TIMEOUT: Duration = Duration::from_mins(3);
const MASTER_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MASTER_CHECK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DIAGNOSTICS: usize = 8192;

#[derive(Default)]
struct State {
  attempts: Mutex<HashMap<String, Attempt>>,
  target_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
  forwards: AsyncMutex<HashMap<String, HashMap<String, PortForwardStatus>>>,
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
  #[error("could not start the OpenSSH control master: {0}")]
  StartMaster(#[source] io::Error),
  #[error("OpenSSH control master timed out during authentication")]
  MasterTimeout,
  #[error("OpenSSH control master exited before becoming ready: {0}")]
  MasterFailed(String),
  #[error("OpenSSH port forwarding failed: {0}")]
  PortForwardFailed(String),
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
    ClientMessage::MasterStatus { target } => master_status(&mut stream, &target).await,
    ClientMessage::DeleteCredentials { target } => delete_credentials(&mut stream, &target).await,
    ClientMessage::ConfigurePortForward {
      target,
      forward,
      enabled,
    } => configure_port_forward(&mut stream, &state, target, forward, enabled).await,
    ClientMessage::ListPortForwards { target } => {
      list_port_forwards(&mut stream, &state, &target).await
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
    _ => Err(RequestError::InvalidRequest("protocol handshake required")),
  }
}

async fn ensure_master(
  stream: &mut ctld_ipc::Stream,
  state: Arc<State>,
  target: SshTarget,
) -> Result<(), RequestError> {
  validate_target(&target)?;
  let target_lock = {
    let mut locks = state.target_locks.lock().unwrap();
    Arc::clone(
      locks
        .entry(target.destination.clone())
        .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
  };
  let _target_guard = target_lock.lock().await;
  let control_path = control_path(&target);
  if control_master_is_ready(&target, &control_path).await {
    return ctld_ipc::write_frame(stream, &ServerMessage::MasterReady { control_path })
      .await
      .map_err(Into::into);
  }
  prepare_control_path(&control_path)?;

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
  let mut child = start_master(&target, &control_path, &token)?;
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
    if control_master_is_ready(&target, &control_path).await {
      #[cfg(target_os = "macos")]
      handle_save_offer(stream, &target, &mut captured).await?;
      #[cfg(not(target_os = "macos"))]
      captured.clear();
      activate_configured_forwards(&state, &target, &control_path).await;
      ctld_ipc::write_frame(stream, &ServerMessage::MasterReady { control_path }).await?;
      return Ok(());
    }
    if let Some(status) = child.try_wait().map_err(RequestError::StartMaster)?
      && !status.success()
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
      let _ = child.kill().await;
      return Err(RequestError::MasterTimeout);
    }
    tokio::select! {
      prompt = prompt_rx.recv() => {
        let Some(prompt) = prompt else {
          let _ = child.kill().await;
          return Err(RequestError::ClientClosed);
        };
        answer_prompt(
          stream,
          &target,
          prompt,
          &mut attempted_stored,
          &mut captured,
        ).await?;
      }
      () = sleep(MASTER_POLL_INTERVAL) => {}
    }
  }
}

impl RequestError {
  fn code(&self) -> &'static str {
    match self {
      Self::Codec(_) | Self::ClientClosed => "ctld_connection_error",
      Self::InvalidRequest(_) => "ctld_protocol_error",
      Self::StartMaster(_) => "ssh_start_failed",
      Self::MasterTimeout => "ssh_timeout",
      Self::MasterFailed(_) => "ssh_authentication_failed",
      Self::PortForwardFailed(_) => "ssh_port_forward_failed",
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
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let path = control_path(target);
  let message = if control_master_is_ready(target, &path).await {
    ServerMessage::MasterReady { control_path: path }
  } else {
    ServerMessage::AuthenticationRequired
  };
  ctld_ipc::write_frame(stream, &message).await?;
  Ok(())
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
  let target_lock = {
    let mut locks = state.target_locks.lock().unwrap();
    Arc::clone(
      locks
        .entry(target.destination.clone())
        .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
  };
  let _target_guard = target_lock.lock().await;
  let control_path = control_path(&target);
  let existing = state
    .forwards
    .lock()
    .await
    .get(&target.destination)
    .and_then(|forwards| forwards.get(&forward.forward_id))
    .cloned();

  if !enabled {
    if let Some(existing) = existing
      && existing.state == PortForwardState::Active
      && control_master_is_ready(&target, &control_path).await
    {
      run_forward_command(&target, &control_path, &existing.forward, true).await?;
    }
    let mut forwards = state.forwards.lock().await;
    if let Some(items) = forwards.get_mut(&target.destination) {
      items.remove(&forward.forward_id);
      if items.is_empty() {
        forwards.remove(&target.destination);
      }
    }
    let status = PortForwardStatus {
      forward,
      state: PortForwardState::WaitingForAuthentication,
      message: None,
    };
    ctld_ipc::write_frame(stream, &ServerMessage::PortForwardConfigured { status }).await?;
    return Ok(());
  }

  if let Some(existing) = &existing
    && existing.forward == forward
    && existing.state == PortForwardState::Active
    && control_master_is_ready(&target, &control_path).await
  {
    ctld_ipc::write_frame(
      stream,
      &ServerMessage::PortForwardConfigured {
        status: existing.clone(),
      },
    )
    .await?;
    return Ok(());
  }

  if let Some(existing) = existing
    && existing.forward != forward
    && existing.state == PortForwardState::Active
    && control_master_is_ready(&target, &control_path).await
  {
    run_forward_command(&target, &control_path, &existing.forward, true).await?;
  }
  let status = if control_master_is_ready(&target, &control_path).await {
    forward_status(
      forward.clone(),
      run_forward_command(&target, &control_path, &forward, false).await,
    )
  } else {
    PortForwardStatus {
      forward: forward.clone(),
      state: PortForwardState::WaitingForAuthentication,
      message: Some("Connect this host to activate the forward.".into()),
    }
  };
  state
    .forwards
    .lock()
    .await
    .entry(target.destination.clone())
    .or_default()
    .insert(forward.forward_id.clone(), status.clone());
  ctld_ipc::write_frame(stream, &ServerMessage::PortForwardConfigured { status }).await?;
  Ok(())
}

async fn list_port_forwards(
  stream: &mut ctld_ipc::Stream,
  state: &State,
  target: &SshTarget,
) -> Result<(), RequestError> {
  validate_target(target)?;
  let ready = control_master_is_ready(target, &control_path(target)).await;
  let mut forwards = state.forwards.lock().await;
  let mut items = forwards.get_mut(&target.destination);
  if !ready && let Some(items) = items.as_deref_mut() {
    for status in items.values_mut() {
      status.state = PortForwardState::WaitingForAuthentication;
      status.message = Some("Connect this host to activate the forward.".into());
    }
  }
  let mut statuses: Vec<_> = items
    .map(|items| items.values().cloned().collect())
    .unwrap_or_default();
  drop(forwards);
  statuses.sort_by(|left, right| left.forward.forward_id.cmp(&right.forward.forward_id));
  ctld_ipc::write_frame(stream, &ServerMessage::PortForwards { statuses }).await?;
  Ok(())
}

async fn activate_configured_forwards(state: &State, target: &SshTarget, control_path: &Path) {
  let configured: Vec<_> = state
    .forwards
    .lock()
    .await
    .get(&target.destination)
    .map(|items| {
      items
        .values()
        .map(|status| status.forward.clone())
        .collect()
    })
    .unwrap_or_default();
  for forward in configured {
    let status = forward_status(
      forward.clone(),
      run_forward_command(target, control_path, &forward, false).await,
    );
    if let Some(items) = state.forwards.lock().await.get_mut(&target.destination) {
      items.insert(forward.forward_id.clone(), status);
    }
  }
}

fn forward_status(
  forward: LocalPortForward,
  result: Result<(), RequestError>,
) -> PortForwardStatus {
  match result {
    Ok(()) => PortForwardStatus {
      forward,
      state: PortForwardState::Active,
      message: None,
    },
    Err(error) => PortForwardStatus {
      forward,
      state: PortForwardState::Error,
      message: Some(error.to_string()),
    },
  }
}

async fn run_forward_command(
  target: &SshTarget,
  control_path: &Path,
  forward: &LocalPortForward,
  cancel: bool,
) -> Result<(), RequestError> {
  let specification = format!(
    "{}:{}:{}:{}",
    forward.bind_address, forward.local_port, forward.remote_host, forward.remote_port
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

fn start_master(
  target: &SshTarget,
  control_path: &Path,
  token: &str,
) -> Result<Child, RequestError> {
  let current_executable = std::env::current_exe().map_err(RequestError::StartMaster)?;
  let mut command = Command::new(SSH_PROGRAM);
  command
    .args(["-f", "-M", "-N", "-T"])
    .args(["-o", "ControlMaster=yes"])
    .args(["-o", &format!("ControlPersist={MASTER_IDLE_SECONDS}")])
    .arg("-S")
    .arg(control_path)
    .args(["-o", "ClearAllForwardings=yes"])
    .args(["-o", "ForwardAgent=no"])
    .args(["-o", "ForwardX11=no"])
    .args(["-o", "PermitLocalCommand=no"])
    .args(["-o", "RemoteCommand=none"])
    .args(["-o", "BatchMode=no"])
    .args(["-o", "StrictHostKeyChecking=ask"]);
  append_target_arguments(&mut command, target);
  command
    .env("SSH_ASKPASS", current_executable)
    .env("SSH_ASKPASS_REQUIRE", "force")
    .env("DISPLAY", "ctld-askpass")
    .env("CTLD_ASKPASS", "1")
    .env("CTLD_ASKPASS_TOKEN", token)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .map_err(RequestError::StartMaster)
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
  if let Some(port) = target.port {
    command.args(["-p", &port.to_string()]);
  }
  if let Some(user) = &target.user {
    command.args(["-l", user]);
  }
  if let Some(identity_file) = &target.identity_file {
    command.arg("-i").arg(identity_file);
  }
  command
    .arg("--")
    .arg(target.hostname.as_deref().unwrap_or(&target.destination));
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
  hasher.update(target.destination.as_bytes());
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
  if target.destination.trim().is_empty()
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
  Ok(())
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

  fn target() -> SshTarget {
    SshTarget {
      destination: "work".into(),
      hostname: Some("example.test".into()),
      user: Some("alice".into()),
      port: Some(2222),
      identity_file: Some(PathBuf::from("/keys/work key")),
    }
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
    assert_eq!(first, control_path_for_socket(&same_alias, daemon_socket));
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
