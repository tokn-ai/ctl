//! `ctld` owns OpenSSH authentication, multiplexing, and credential storage.
//! This module only forwards its attempt-scoped prompts to the Tauri UI.

mod broker;
pub mod commands;
mod verification;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use ctl_core::{ConnectionTarget, SshInteraction, Transport, open_identified_ssh_service};
use serde::Serialize;
use tauri::ipc::Channel;
use tokio::sync::{oneshot, watch};
use zeroize::Zeroizing;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

#[derive(Default)]
struct Registry {
  attempts: HashMap<(String, String), Arc<Attempt>>,
}

fn registry() -> &'static Mutex<Registry> {
  static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
  REGISTRY.get_or_init(Mutex::default)
}

struct Attempt {
  target: ctld_ipc::SshTarget,
  cancel: watch::Sender<bool>,
  responses: Mutex<HashMap<String, oneshot::Sender<Option<Zeroizing<String>>>>>,
}

#[derive(Clone)]
struct PromptContext {
  attempt: Arc<Attempt>,
  channel: Channel<SshPromptDto>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SshPromptKind {
  Confirm,
  Secret,
  CredentialSave,
  CredentialSaveError,
}

#[derive(Clone, Serialize)]
pub struct SshPromptDto {
  prompt_id: String,
  kind: SshPromptKind,
  message: String,
}

async fn request_response(
  context: Option<&PromptContext>,
  kind: SshPromptKind,
  message: String,
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
    kind,
    message,
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
  connect_with(target, None).await.map(|(stream, _)| stream)
}

async fn connect_with(
  target: &ConnectionTargetDto,
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
  let control_path = if let Some(context) = prompts.as_ref() {
    broker::ensure_master(target, context).await?
  } else {
    broker::existing_master(target).await?
  };
  let interaction = SshInteraction::Multiplexed { control_path };
  let stream = open_identified_ssh_service(
    &destination,
    &options,
    &interaction,
    ctl_core::RemoteService::Rmux,
  )
  .await
  .map_err(|error| CommandErrorDto::transport(&error))?;
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
  restart_check: bool,
) -> CommandResult<ctl_proto::RemoteIdentity> {
  let key = (window, attempt_id);
  let (cancel, mut cancelled) = watch::channel(false);
  let attempt = Arc::new(Attempt {
    target: broker::broker_target(&target)?,
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
  let context = PromptContext { attempt, channel };
  let establish = async {
    let (stream, identity) = connect_with(&target, Some(context)).await?;
    if restart_check {
      require_restart_support(&identity)?;
    } else {
      verification::verify(stream).await?;
    }
    Ok(identity)
  };
  let result = tokio::select! {
    result = tokio::time::timeout(Duration::from_mins(3), establish) => {
      result.map_err(|_| CommandErrorDto::new("ssh_timeout", "SSH connection timed out."))?
    }
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "SSH connection cancelled.")),
  };
  result
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
    target: broker::broker_target(&target)?,
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
  let context = PromptContext {
    attempt: Arc::clone(&attempt),
    channel,
  };
  let install = async {
    let control_path = broker::ensure_master(&target, &context).await?;
    let interaction = SshInteraction::Multiplexed { control_path };
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
    crate::remote_agent::install(
      &app,
      &destination,
      &options,
      &interaction,
      on_progress,
      || !attempt.responses.lock().unwrap().is_empty(),
    )
    .await
  };
  let result = tokio::select! {
    result = install => result,
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "SSH connection cancelled.")),
  };
  result
}

fn require_restart_support(identity: &ctl_proto::RemoteIdentity) -> CommandResult<()> {
  if identity.rmux_restart_supported {
    return Ok(());
  }
  Err(CommandErrorDto::new(
    "remote_restart_unsupported",
    "The installed ctl-agent does not support remote restart. Install a current component bundle, or restart rmuxd manually on the host.",
  ))
}

/// Called only after the UI's destructive restart confirmation.
pub async fn restart_rmux(
  window: String,
  attempt_id: String,
  target: ConnectionTargetDto,
  channel: Channel<SshPromptDto>,
) -> CommandResult<ctl_proto::RemoteRmuxRestartResult> {
  let key = (window, attempt_id);
  let (cancel, mut cancelled) = watch::channel(false);
  let attempt = Arc::new(Attempt {
    target: broker::broker_target(&target)?,
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
  let context = PromptContext { attempt, channel };
  let restart = async {
    // Identity discovery does not perform a session-protocol handshake.
    let (stream, identity) = connect_with(&target, Some(context)).await?;
    require_restart_support(&identity)?;
    drop(stream);
    let control_path = broker::existing_master(&target).await?;
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
    ctl_core::restart_ssh_rmux_interactive(
      &destination,
      &options,
      &SshInteraction::Multiplexed { control_path },
      &identity.remote_id,
    )
    .await
    .map_err(|error| CommandErrorDto::new("remote_daemon_restart_failed", error.to_string()))
  };
  tokio::select! {
    result = tokio::time::timeout(Duration::from_mins(3), restart) => {
      result.map_err(|_| CommandErrorDto::new("ssh_timeout", "Restart timed out. The daemon may have restarted; reconnect to check its state."))?
    }
    _ = cancelled.changed() => Err(CommandErrorDto::new("ssh_cancelled", "Stopped waiting for restart. The daemon may still restart; reconnect to check its state.")),
  }
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

pub async fn disconnect(targets: &[ConnectionTargetDto]) -> CommandResult<()> {
  let targets_to_cancel: std::collections::HashSet<_> = targets
    .iter()
    .filter_map(|target| broker::broker_target(target).ok())
    .collect();
  {
    let registry = registry().lock().unwrap();
    for attempt in registry.attempts.values() {
      if targets_to_cancel.contains(&attempt.target) {
        let _ = attempt.cancel.send(true);
      }
    }
  }
  broker::disconnect(targets).await
}

pub fn cancel_window(window: &str) {
  for ((owner, _), attempt) in &registry().lock().unwrap().attempts {
    if owner == window {
      let _ = attempt.cancel.send(true);
    }
  }
}

pub async fn forget(target: &ConnectionTargetDto) -> CommandResult<()> {
  broker::delete_credentials(target).await
}

#[cfg(test)]
mod tests;
