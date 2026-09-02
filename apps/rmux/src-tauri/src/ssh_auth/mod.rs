//! OpenSSH owns authentication and host verification. This module only supplies
//! its askpass UI through an ephemeral owner-only socket; no secret is persisted.

pub mod commands;
mod helper;
mod verification;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ctl_core::{ConnectionTarget, SshInteraction, Transport, open_ssh_tunnel_interactive};
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
  credentials: HashMap<ConnectionTargetDto, Secrets>,
  attempts: HashMap<(String, String), Arc<Attempt>>,
}

fn registry() -> &'static Mutex<Registry> {
  static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
  REGISTRY.get_or_init(Mutex::default)
}

struct Attempt {
  cancel: watch::Sender<bool>,
  responses: Mutex<HashMap<String, oneshot::Sender<Option<String>>>>,
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
  fn start(secrets: Secrets, prompts: Option<PromptContext>) -> CommandResult<Self> {
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
        // An explicit interactive attempt must ask again after a rejected
        // password, rather than automatically repeating the incorrect value.
        let cached = if cacheable && prompts.is_none() {
          secrets.lock().unwrap().get(&request.message).cloned()
        } else {
          None
        };
        let response = match cached {
          Some(secret) => Some(secret),
          None => ask(prompts.as_ref(), &request).await.map(Zeroizing::new),
        };
        if cacheable && let Some(secret) = &response {
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

fn cacheable_prompt(message: &str) -> bool {
  let lower = message.to_lowercase();
  lower.contains("password:") || lower.starts_with("enter passphrase for key")
}

async fn ask(context: Option<&PromptContext>, request: &HelperRequest) -> Option<String> {
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
  let secrets = registry().lock().unwrap().credentials.get(target).cloned();
  connect_with(target, secrets, None).await
}

async fn connect_with(
  target: &ConnectionTargetDto,
  secrets: Option<Secrets>,
  prompts: Option<PromptContext>,
) -> CommandResult<Transport> {
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
    .map(|secrets| Bridge::start(secrets, prompts))
    .transpose()?;
  let interaction = bridge
    .as_ref()
    .map(Bridge::interaction)
    .transpose()?
    .unwrap_or(SshInteraction::Batch);
  let stream = open_ssh_tunnel_interactive(&destination, &options, &interaction)
    .await
    .map_err(|error| CommandErrorDto::transport(&error))?;
  // Authentication is complete; removing the bridge also closes the secret-delivery capability.
  drop(bridge);
  Ok(Transport::Ssh(stream))
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
) -> CommandResult<()> {
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
  // Fresh prompts on an explicit retry avoid replaying an incorrect cached password.
  let secrets = Secrets::default();
  let context = PromptContext { attempt, channel };
  let establish = async {
    let stream = connect_with(&target, Some(secrets.clone()), Some(context)).await?;
    verification::verify(stream).await
  };
  let result = tokio::select! {
    result = tokio::time::timeout(Duration::from_mins(3), establish) => {
      result.map_err(|_| CommandErrorDto::new("ssh_timeout", "SSH connection timed out."))?
    }
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "SSH connection cancelled.")),
  };
  result?;
  registry()
    .lock()
    .unwrap()
    .credentials
    .insert(target, secrets);
  Ok(())
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
  let _ = sender.send(response);
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

pub fn forget(target: &ConnectionTargetDto) {
  registry().lock().unwrap().credentials.remove(target);
}

pub fn remember_configured_alias(definition: &crate::ssh_config::SshHostDefinition) {
  let source = ConnectionTargetDto::Ssh {
    destination: definition.alias.clone(),
    hostname: Some(definition.hostname.clone()),
    user: definition.user.clone(),
    port: definition.port,
    identity_file: definition.identity_file.clone(),
  };
  let alias = ConnectionTargetDto::Ssh {
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

#[cfg(test)]
mod tests;
