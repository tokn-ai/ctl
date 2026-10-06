//! One cancellable update operation across local and saved remote hosts.
// Tauri extracts owned command arguments from IPC.
#![allow(clippy::needless_pass_by_value)]

use crate::dto::{ConnectionTargetDto, RemoteAgentInstallProgressDto};
use crate::error::{CommandErrorDto, CommandResult};
use ctl_core::component_update::{UpdateOptions, UpdateResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, WebviewWindow, ipc::Channel};
use tokio::sync::watch;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
  targets: Vec<ConnectionTargetDto>,
  attempt_id: String,
  options: UpdateOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum State {
  Updating,
  Complete,
  Failed,
  Cancelled,
}

#[derive(Debug, Serialize)]
pub struct UpdateProgress {
  host_index: usize,
  state: State,
  progress: Option<RemoteAgentInstallProgressDto>,
}

#[derive(Debug, Serialize)]
pub struct HostResult {
  host_index: usize,
  state: State,
  result: Option<UpdateResult>,
  error: Option<String>,
}

#[derive(Deserialize)]
pub struct CancelRequest {
  attempt_id: String,
}

type Key = (String, String);
#[derive(Default)]
struct Registry {
  active: BTreeMap<Key, watch::Sender<bool>>,
  pending_cancel: BTreeMap<Key, Instant>,
}
impl Registry {
  fn register(&mut self, key: Key, cancel: watch::Sender<bool>) -> CommandResult<()> {
    if self.active.contains_key(&key) {
      return Err(CommandErrorDto::new(
        "component_update_exists",
        "This update is already running.",
      ));
    }
    if self
      .pending_cancel
      .remove(&key)
      .is_some_and(|at| at.elapsed() < Duration::from_mins(1))
    {
      cancel.send_replace(true);
    }
    self.active.insert(key, cancel);
    Ok(())
  }

  fn cancel(&mut self, key: Key) {
    if let Some(cancel) = self.active.get(&key) {
      cancel.send_replace(true);
    } else if !key.1.is_empty() && key.1.len() <= 128 {
      // An immediate Stop can arrive before the async update registers itself.
      self
        .pending_cancel
        .retain(|_, at| at.elapsed() < Duration::from_mins(1));
      if self.pending_cancel.len() < 64 {
        self.pending_cancel.insert(key, Instant::now());
      }
    }
  }
}
fn registry() -> &'static Mutex<Registry> {
  static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
  REGISTRY.get_or_init(Mutex::default)
}

struct Guard(Key);
impl Drop for Guard {
  fn drop(&mut self) {
    registry().lock().unwrap().active.remove(&self.0);
  }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn update_components(
  app: AppHandle,
  window: WebviewWindow,
  request: UpdateRequest,
  on_prompt: Channel<crate::ssh_auth::SshPromptDto>,
  on_progress: Channel<UpdateProgress>,
) -> CommandResult<Vec<HostResult>> {
  #[cfg(unix)]
  {
    if request.targets.is_empty()
      || request.targets.len() > 64
      || request.attempt_id.is_empty()
      || request.attempt_id.len() > 128
    {
      return Err(CommandErrorDto::new(
        "invalid_component_update",
        "Choose between one and 64 hosts.",
      ));
    }
    let key = (window.label().to_owned(), request.attempt_id.clone());
    let (cancel, mut cancelled) = watch::channel(false);
    registry().lock().unwrap().register(key.clone(), cancel)?;
    let _guard = Guard(key);
    let context = Context {
      app: &app,
      window: window.label(),
      attempt_id: &request.attempt_id,
      on_prompt,
      on_progress: &on_progress,
    };
    run_batch(
      request.targets,
      &mut cancelled,
      |host_index, target| context.update_host(host_index, target, request.options.clone()),
      |host_index, state| {
        on_progress
          .send(UpdateProgress {
            host_index,
            state,
            progress: None,
          })
          .map_err(CommandErrorDto::backend)
      },
    )
    .await
  }
  #[cfg(not(unix))]
  {
    let _ = (app, window, request, on_prompt, on_progress);
    Err(CommandErrorDto::new(
      "component_update_unsupported",
      "Component updates currently require macOS or Linux.",
    ))
  }
}

#[cfg(unix)]
struct Context<'a> {
  app: &'a AppHandle,
  window: &'a str,
  attempt_id: &'a str,
  on_prompt: Channel<crate::ssh_auth::SshPromptDto>,
  on_progress: &'a Channel<UpdateProgress>,
}
#[cfg(unix)]
impl Context<'_> {
  async fn update_host(
    &self,
    host_index: usize,
    target: ConnectionTargetDto,
    options: UpdateOptions,
  ) -> CommandResult<UpdateResult> {
    let Self {
      app,
      window,
      attempt_id,
      on_prompt,
      on_progress,
    } = self;
    match target {
      ConnectionTargetDto::Local => {
        let home = dirs::home_dir()
          .ok_or_else(|| CommandErrorDto::backend("Home directory is unavailable."))?;
        let bundle = ctl_client::component_update::prepare(
          &home,
          ctl_core::paths::native_target(),
          ctl_core::bundles::Purpose::Local,
          &options.source,
          &crate::remote_agent::bundle_directories(app)?,
        )
        .await
        .map_err(CommandErrorDto::backend)?;
        ctl_client::component_update::install_local(&home, &bundle, options.package)
          .await
          .map_err(CommandErrorDto::backend)
      }
      target @ ConnectionTargetDto::Ssh { .. } => {
        let installed = crate::ssh_auth::install_components(
          (*app).clone(),
          (*window).into(),
          format!("{attempt_id}:{host_index}"),
          target,
          on_prompt.clone(),
          options,
          |progress| {
            on_progress
              .send(UpdateProgress {
                host_index,
                state: State::Updating,
                progress: Some(progress),
              })
              .map_err(CommandErrorDto::backend)
          },
        )
        .await?;
        Ok(installed.result)
      }
    }
  }
}

#[tauri::command]
pub fn cancel_component_update(window: WebviewWindow, request: CancelRequest) {
  let key = (window.label().into(), request.attempt_id);
  registry().lock().unwrap().cancel(key);
}

pub(crate) fn close_window(window: &str) {
  let mut registry = registry().lock().unwrap();
  for ((label, _), cancel) in &registry.active {
    if label == window {
      cancel.send_replace(true);
    }
  }
  registry
    .pending_cancel
    .retain(|(label, _), _| label != window);
}

async fn run_batch<T, F, Fut>(
  targets: Vec<T>,
  cancelled: &mut watch::Receiver<bool>,
  mut update: F,
  report: impl Fn(usize, State) -> CommandResult<()>,
) -> CommandResult<Vec<HostResult>>
where
  F: FnMut(usize, T) -> Fut,
  Fut: std::future::Future<Output = CommandResult<UpdateResult>>,
{
  let mut results = Vec::new();
  for (host_index, target) in targets.into_iter().enumerate() {
    if *cancelled.borrow() {
      results.push(HostResult {
        host_index,
        state: State::Cancelled,
        result: None,
        error: None,
      });
      continue;
    }
    report(host_index, State::Updating)?;
    let result = tokio::select! {
      biased;
      _ = cancelled.changed() => None,
      result = update(host_index, target) => Some(result),
    };
    let outcome = match result {
      Some(Ok(result)) => HostResult { host_index, state: State::Complete, result: Some(result), error: None },
      Some(Err(error)) => HostResult { host_index, state: State::Failed, result: None, error: Some(error.message) },
      None => HostResult { host_index, state: State::Cancelled, result: None, error: Some("Stopped waiting. Refresh component status before retrying; an activation may already have completed.".into()) },
    };
    report(host_index, outcome.state)?;
    results.push(outcome);
  }
  Ok(results)
}

#[cfg(test)]
mod tests;
