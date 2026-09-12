//! OpenSSH owns authentication and host verification. This module supplies its
//! askpass UI through an ephemeral owner-only socket. macOS stores verified
//! reusable secrets in the device-local, Touch ID-protected Keychain.

pub mod commands;
mod helper;
#[cfg(target_os = "macos")]
mod keychain;
mod verification;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ctl_core::{ConnectionTarget, SshInteraction, Transport, open_identified_ssh_service};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

pub use helper::helper_exit_code;

type Secrets = Arc<Mutex<HashMap<String, Zeroizing<String>>>>;

#[derive(Default)]
struct Registry {
  #[cfg(not(target_os = "macos"))]
  credentials: HashMap<ConnectionTargetDto, Secrets>,
  attempts: HashMap<(String, String), Arc<Attempt>>,
}

fn registry() -> &'static Mutex<Registry> {
  static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
  REGISTRY.get_or_init(Mutex::default)
}

struct Attempt {
  cancel: watch::Sender<bool>,
  responses: Mutex<HashMap<String, oneshot::Sender<Option<Zeroizing<String>>>>>,
}

#[derive(Clone)]
struct PromptContext {
  attempt: Arc<Attempt>,
  channel: Channel<SshPromptDto>,
}

#[derive(Clone, Serialize)]
pub struct SshPromptDto {
  prompt_id: String,
  kind: String,
  message: String,
}

#[derive(Deserialize, Serialize)]
struct HelperRequest {
  token: String,
  message: String,
  confirm: bool,
}

struct Bridge {
  directory: PathBuf,
  socket: PathBuf,
  token: String,
  task: JoinHandle<()>,
}

impl Drop for Bridge {
  fn drop(&mut self) {
    self.task.abort();
    let _ = std::fs::remove_file(&self.socket);
    let _ = std::fs::remove_dir(&self.directory);
  }
}

impl Bridge {
  fn start(
    secrets: Secrets,
    credential_target: Option<ConnectionTargetDto>,
    prompts: Option<PromptContext>,
    reuse_cached_secrets: bool,
  ) -> CommandResult<Self> {
    use std::os::unix::fs::DirBuilderExt as _;
    // macOS's per-user temp path can exceed sockaddr_un's 104-byte limit.
    // An unpredictable owner-only directory keeps the short socket path private.
    let directory = PathBuf::from("/tmp").join(format!("rmux-askpass-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new()
      .mode(0o700)
      .create(&directory)
      .map_err(CommandErrorDto::backend)?;
    let socket = directory.join("prompt.sock");
    let listener = match UnixListener::bind(&socket) {
      Ok(listener) => listener,
      Err(error) => {
        let _ = std::fs::remove_dir(&directory);
        return Err(CommandErrorDto::backend(error));
      }
    };
    let token = uuid::Uuid::new_v4().to_string();
    let expected_token = token.clone();
    let task = tokio::spawn(async move {
      let mut attempted_stored_secrets = HashSet::new();
      while let Ok((stream, _)) = listener.accept().await {
        let (reader, mut writer) = stream.into_split();
        let mut line = String::new();
        let read = tokio::time::timeout(
          Duration::from_secs(5),
          BufReader::new(reader.take(16_384)).read_line(&mut line),
        )
        .await;
        if !matches!(read, Ok(Ok(_))) {
          continue;
        }
        let Ok(request) = serde_json::from_str::<HelperRequest>(&line) else {
          continue;
        };
        if request.token != expected_token {
          continue;
        }
        let cacheable = !request.confirm && cacheable_prompt(&request.message);
        // Try a stored secret only once so a rejected value is not replayed.
        let should_try_stored_secret = cacheable
          && reuse_cached_secrets
          && attempted_stored_secrets.insert(request.message.clone());
        #[cfg(target_os = "macos")]
        let cached = if should_try_stored_secret {
          stored_secret(credential_target.as_ref(), &request.message, &secrets).await
        } else {
          Ok(None)
        };
        #[cfg(not(target_os = "macos"))]
        let cached: CommandResult<Option<Zeroizing<String>>> = Ok(if should_try_stored_secret {
          stored_secret(credential_target.as_ref(), &request.message, &secrets)
        } else {
          None
        });
        let (response, newly_entered) = match cached {
          Ok(Some(secret)) => (Some(secret), false),
          Ok(None) => (ask(prompts.as_ref(), &request).await, true),
          // A denied or cancelled Touch ID request must not fall back to an
          // application password field.
          Err(_) => (None, false),
        };
        if cacheable
          && newly_entered
          && let Some(secret) = &response
        {
          secrets
            .lock()
            .unwrap()
            .insert(request.message, secret.clone());
        }
        let response = response.as_ref().map(|secret| secret.as_str());
        if let Ok(mut encoded) = serde_json::to_vec(&response).map(Zeroizing::new) {
          encoded.push(b'\n');
          let _ = writer.write_all(&encoded).await;
        }
      }
    });
    Ok(Self {
      directory,
      socket,
      token,
      task,
    })
  }

  fn interaction(&self) -> CommandResult<SshInteraction> {
    Ok(SshInteraction::Askpass {
      program: std::env::current_exe().map_err(CommandErrorDto::backend)?,
      socket: self.socket.clone(),
      token: self.token.clone(),
    })
  }
}

#[cfg(target_os = "macos")]
async fn stored_secret(
  target: Option<&ConnectionTargetDto>,
  prompt: &str,
  secrets: &Secrets,
) -> CommandResult<Option<Zeroizing<String>>> {
  let Some(target) = target.cloned() else {
    return Ok(secrets.lock().unwrap().get(prompt).cloned());
  };
  let prompt = prompt.to_owned();
  tokio::task::spawn_blocking(move || keychain::load(&target, &prompt))
    .await
    .map_err(CommandErrorDto::backend)?
}

#[cfg(not(target_os = "macos"))]
fn stored_secret(
  _target: Option<&ConnectionTargetDto>,
  prompt: &str,
  secrets: &Secrets,
) -> Option<Zeroizing<String>> {
  secrets.lock().unwrap().get(prompt).cloned()
}

fn cacheable_prompt(message: &str) -> bool {
  let lower = message.to_lowercase();
  lower.contains("password:") || lower.starts_with("enter passphrase for key")
}

async fn ask(
  context: Option<&PromptContext>,
  request: &HelperRequest,
) -> Option<Zeroizing<String>> {
  let context = context?;
  let prompt_id = uuid::Uuid::new_v4().to_string();
  let (sender, receiver) = oneshot::channel();
  context
    .attempt
    .responses
    .lock()
    .unwrap()
    .insert(prompt_id.clone(), sender);
  let sent = context.channel.send(SshPromptDto {
    prompt_id: prompt_id.clone(),
    kind: if request.confirm { "confirm" } else { "secret" }.into(),
    message: request.message.clone(),
  });
  let response = if sent.is_ok() {
    tokio::time::timeout(Duration::from_mins(2), receiver)
      .await
      .ok()
      .and_then(Result::ok)
      .flatten()
  } else {
    None
  };
  context.attempt.responses.lock().unwrap().remove(&prompt_id);
  response
}

pub async fn connect(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  #[cfg(target_os = "macos")]
  let secrets = Some(Secrets::default());
  #[cfg(not(target_os = "macos"))]
  let secrets = registry()
    .lock()
    .unwrap()
    .credentials
    .get(&target.credential_key())
    .cloned();
  connect_with(target, secrets, None)
    .await
    .map(|(stream, _)| stream)
}

async fn connect_with(
  target: &ConnectionTargetDto,
  secrets: Option<Secrets>,
  prompts: Option<PromptContext>,
) -> CommandResult<(Transport, ctl_proto::RemoteIdentity)> {
  let ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select a remote SSH host.",
    ));
  };
  let bridge = secrets
    .map(|secrets| {
      let reuse_cached_secrets = prompts.is_none() || cfg!(target_os = "macos");
      Bridge::start(
        secrets,
        Some(target.credential_key()),
        prompts,
        reuse_cached_secrets,
      )
    })
    .transpose()?;
  let interaction = bridge
    .as_ref()
    .map(Bridge::interaction)
    .transpose()?
    .unwrap_or(SshInteraction::Batch);
  let stream = open_identified_ssh_service(
    &destination,
    &options,
    &interaction,
    ctl_core::RemoteService::Rmux,
  )
  .await
  .map_err(|error| CommandErrorDto::transport(&error))?;
  // Authentication is complete; removing the bridge also closes the secret-delivery capability.
  drop(bridge);
  let identity = stream
    .remote_identity
    .as_ref()
    .expect("identified transport")
    .clone();
  target.verify_remote_identity(&identity)?;
  Ok((Transport::Ssh(stream), identity))
}

struct AttemptGuard((String, String));
impl Drop for AttemptGuard {
  fn drop(&mut self) {
    registry().lock().unwrap().attempts.remove(&self.0);
  }
}

pub async fn probe(
  window: String,
  attempt_id: String,
  target: ConnectionTargetDto,
  channel: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  let key = (window, attempt_id);
  let (cancel, mut cancelled) = watch::channel(false);
  let attempt = Arc::new(Attempt {
    cancel,
    responses: Mutex::default(),
  });
  {
    let mut registry = registry().lock().unwrap();
    if registry.attempts.contains_key(&key) {
      return Err(CommandErrorDto::new(
        "ssh_attempt_exists",
        "This connection attempt is already running.",
      ));
    }
    registry.attempts.insert(key.clone(), attempt.clone());
  }
  let _guard = AttemptGuard(key);
  // A stored secret is tried at most once; if OpenSSH rejects it, its next
  // prompt reaches the UI so the user can replace the credential.
  let secrets = Secrets::default();
  let context = PromptContext { attempt, channel };
  let establish = async {
    let (stream, identity) = connect_with(&target, Some(secrets.clone()), Some(context)).await?;
    #[cfg(target_os = "macos")]
    persist_credentials(target.clone(), secrets.clone()).await?;
    verification::verify(stream).await?;
    Ok(identity)
  };
  let result = tokio::select! {
    result = tokio::time::timeout(Duration::from_mins(3), establish) => {
      result.map_err(|_| CommandErrorDto::new("ssh_timeout", "SSH connection timed out."))?
    }
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "SSH connection cancelled.")),
  };
  match result {
    Ok(identity) => {
      #[cfg(not(target_os = "macos"))]
      registry()
        .lock()
        .unwrap()
        .credentials
        .insert(target.credential_key(), secrets);
      Ok(identity)
    }
    Err(error) => {
      #[cfg(not(target_os = "macos"))]
      if matches!(
        error.code.as_str(),
        "ctl_agent_not_found" | "ctl_agent_identity_unsupported"
      ) {
        registry()
          .lock()
          .unwrap()
          .credentials
          .insert(target.credential_key(), secrets);
      }
      Err(error)
    }
  }
}

pub async fn install_agent(
  app: tauri::AppHandle,
  window: String,
  attempt_id: String,
  target: ConnectionTargetDto,
  channel: Channel<SshPromptDto>,
  on_progress: Channel<crate::dto::RemoteAgentInstallProgressDto>,
) -> CommandResult<crate::dto::RemoteAgentInstallResultDto> {
  let key = (window, attempt_id);
  let (cancel, mut cancelled) = watch::channel(false);
  let attempt = Arc::new(Attempt {
    cancel,
    responses: Mutex::default(),
  });
  {
    let mut registry = registry().lock().unwrap();
    if registry.attempts.contains_key(&key) {
      return Err(CommandErrorDto::new(
        "ssh_attempt_exists",
        "This connection attempt is already running.",
      ));
    }
    registry.attempts.insert(key.clone(), attempt.clone());
  }
  let _guard = AttemptGuard(key);
  #[cfg(target_os = "macos")]
  let secrets = Secrets::default();
  #[cfg(not(target_os = "macos"))]
  let secrets = registry()
    .lock()
    .unwrap()
    .credentials
    .get(&target.credential_key())
    .cloned()
    .unwrap_or_default();
  // The preceding probe authenticated successfully before discovering that
  // ctl-agent was absent. Reuse that known-good password or key passphrase;
  // uncached challenges such as one-time codes still reach the prompt channel.
  let context = PromptContext {
    attempt: Arc::clone(&attempt),
    channel,
  };
  let bridge = Bridge::start(
    secrets.clone(),
    Some(target.credential_key()),
    Some(context),
    true,
  )?;
  let interaction = bridge.interaction()?;
  let ConnectionTarget::Ssh {
    destination,
    options,
  } = target.to_core()
  else {
    return Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select a remote SSH host.",
    ));
  };
  let install = crate::remote_agent::install(
    &app,
    &destination,
    &options,
    &interaction,
    on_progress,
    || !attempt.responses.lock().unwrap().is_empty(),
  );
  let result = tokio::select! {
    result = install => result,
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "SSH connection cancelled.")),
  };
  drop(bridge);
  let installed = result?;
  #[cfg(target_os = "macos")]
  persist_credentials(target.clone(), secrets.clone()).await?;
  #[cfg(not(target_os = "macos"))]
  registry()
    .lock()
    .unwrap()
    .credentials
    .insert(target.credential_key(), secrets);
  Ok(installed)
}

pub fn respond(
  window: &str,
  attempt_id: &str,
  prompt_id: &str,
  response: Option<String>,
) -> CommandResult<()> {
  if response
    .as_ref()
    .is_some_and(|value| value.len() > 8192 || value.contains(['\n', '\r', '\0']))
  {
    return Err(CommandErrorDto::new(
      "invalid_ssh_response",
      "SSH response is too long or contains a line break.",
    ));
  }
  let attempt = registry()
    .lock()
    .unwrap()
    .attempts
    .get(&(window.into(), attempt_id.into()))
    .cloned()
    .ok_or_else(|| {
      CommandErrorDto::new("ssh_attempt_expired", "SSH connection attempt has ended.")
    })?;
  let sender = attempt
    .responses
    .lock()
    .unwrap()
    .remove(prompt_id)
    .ok_or_else(|| {
      CommandErrorDto::new(
        "ssh_prompt_expired",
        "SSH prompt has already been answered.",
      )
    })?;
  let _ = sender.send(response.map(Zeroizing::new));
  Ok(())
}

pub fn cancel(window: &str, attempt_id: &str) {
  if let Some(attempt) = registry()
    .lock()
    .unwrap()
    .attempts
    .get(&(window.into(), attempt_id.into()))
  {
    let _ = attempt.cancel.send(true);
  }
}

pub fn cancel_window(window: &str) {
  for ((owner, _), attempt) in &registry().lock().unwrap().attempts {
    if owner == window {
      let _ = attempt.cancel.send(true);
    }
  }
}

pub fn forget(target: &ConnectionTargetDto) -> CommandResult<()> {
  #[cfg(target_os = "macos")]
  return keychain::delete(target);

  #[cfg(not(target_os = "macos"))]
  registry()
    .lock()
    .map_err(|_| {
      CommandErrorDto::new(
        "ssh_credentials_unavailable",
        "SSH credentials are temporarily unavailable.",
      )
    })?
    .credentials
    .remove(&target.credential_key());
  #[cfg(not(target_os = "macos"))]
  Ok(())
}

pub fn remember_configured_alias(definition: &crate::ssh_config::SshHostDefinition) {
  #[cfg(target_os = "macos")]
  let _ = definition;

  #[cfg(not(target_os = "macos"))]
  {
    let source = ConnectionTargetDto::Ssh {
      remote_info: None,
      destination: definition.alias.clone(),
      hostname: Some(definition.hostname.clone()),
      user: definition.user.clone(),
      port: definition.port,
      identity_file: definition.identity_file.clone(),
    };
    let alias = ConnectionTargetDto::Ssh {
      remote_info: None,
      destination: definition.alias.clone(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
    };
    let mut registry = registry().lock().unwrap();
    if let Some(secrets) = registry.credentials.remove(&source) {
      registry.credentials.insert(alias, secrets);
    }
  }
}

#[cfg(target_os = "macos")]
async fn persist_credentials(target: ConnectionTargetDto, secrets: Secrets) -> CommandResult<()> {
  if secrets.lock().unwrap().is_empty() {
    return Ok(());
  }
  tokio::task::spawn_blocking(move || keychain::save(&target, &secrets))
    .await
    .map_err(CommandErrorDto::backend)?
}

#[cfg(test)]
mod tests;
