//! App-owned workspace metadata. Never connects to a daemon or stores runtime state.

#[cfg(all(test, unix))]
mod remote_test;
mod repository;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use tauri::Manager as _;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHost {
  pub host_id: String,
  pub target: ConnectionTargetDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReference {
  pub host_id: String,
  pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSession {
  pub host_id: String,
  pub session_id: String,
  pub name: String,
  pub last_known_cwd: Option<String>,
  pub last_known_cwd_display: Option<String>,
}

impl WorkspaceSession {
  fn reference(&self) -> SessionReference {
    SessionReference {
      host_id: self.host_id.clone(),
      session_id: self.session_id.clone(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceTab {
  Session { host_id: String, session_id: String },
  Task { host_id: String, task_id: String },
  TaskDefinition { definition_id: String },
}
impl From<SessionReference> for WorkspaceTab {
  fn from(reference: SessionReference) -> Self {
    Self::Session {
      host_id: reference.host_id,
      session_id: reference.session_id,
    }
  }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTaskDefinition {
  pub definition_id: String,
  pub revision: String,
  pub definition: task_proto::TaskDefinition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskReference {
  pub host_id: String,
  pub task_id: String,
  pub definition_id: Option<String>,
  pub applied_revision: Option<String>,
  pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDocument {
  pub schema_version: u32,
  pub workspace_id: String,
  pub hosts: Vec<WorkspaceHost>,
  pub sessions: Vec<WorkspaceSession>,
  pub tabs: Vec<WorkspaceTab>,
  pub active_tab: Option<WorkspaceTab>,
  #[serde(default)]
  pub task_definitions: Vec<SavedTaskDefinition>,
  #[serde(default)]
  pub task_references: Vec<TaskReference>,
}

impl Default for WorkspaceDocument {
  fn default() -> Self {
    Self {
      schema_version: 2,
      workspace_id: "default".into(),
      hosts: vec![WorkspaceHost {
        host_id: "local".into(),
        target: ConnectionTargetDto::Local,
      }],
      sessions: Vec::new(),
      task_definitions: Vec::new(),
      task_references: Vec::new(),
      tabs: Vec::new(),
      active_tab: None,
    }
  }
}

impl WorkspaceDocument {
  fn validate(&self) -> CommandResult<()> {
    if self.schema_version != 2 {
      return Err(CommandErrorDto::new(
        "workspace_version_unsupported",
        "This workspace was written by another app version. Its file has not been changed.",
      ));
    }
    let invalid = || {
      CommandErrorDto::new(
        "workspace_invalid",
        "The workspace contains invalid or duplicate references.",
      )
    };
    let valid_text =
      |text: &str| !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control);
    if !valid_text(&self.workspace_id) || self.hosts.len() > 1024 || self.sessions.len() > 10_000 {
      return Err(invalid());
    }
    let mut hosts = HashSet::new();
    let mut destinations = HashSet::new();
    for host in &self.hosts {
      if !valid_text(&host.host_id) || !hosts.insert(host.host_id.as_str()) {
        return Err(invalid());
      }
      match &host.target {
        ConnectionTargetDto::Local if host.host_id == "local" => {}
        ConnectionTargetDto::Ssh {
          destination,
          hostname,
          user,
          port,
          identity_file,
        } => {
          if host.host_id == "local"
            || !valid_text(destination)
            || !destinations.insert(destination)
            || *port == Some(0)
            || [hostname, user, identity_file]
              .into_iter()
              .flatten()
              .any(|value| !valid_text(value))
          {
            return Err(invalid());
          }
        }
        ConnectionTargetDto::Local => return Err(invalid()),
      }
    }
    if !hosts.contains("local") {
      return Err(invalid());
    }
    let mut sessions = HashSet::new();
    for session in &self.sessions {
      if !hosts.contains(session.host_id.as_str())
        || !valid_text(&session.session_id)
        || !valid_text(&session.name)
        || [
          session.last_known_cwd.as_ref(),
          session.last_known_cwd_display.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_text(value))
        || !sessions.insert(session.reference())
      {
        return Err(invalid());
      }
    }
    self.validate_task_tabs(&hosts, &sessions)?;
    let mut tabs = HashSet::new();
    if self.tabs.iter().any(|tab| !tabs.insert(tab))
      || self
        .active_tab
        .as_ref()
        .is_some_and(|tab| !tabs.contains(tab))
    {
      return Err(invalid());
    }
    Ok(())
  }
  fn validate_task_tabs(
    &self,
    hosts: &HashSet<&str>,
    sessions: &HashSet<SessionReference>,
  ) -> CommandResult<()> {
    let invalid = || {
      CommandErrorDto::new(
        "workspace_invalid",
        "Invalid task definitions or references.",
      )
    };
    let valid_text =
      |text: &str| !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control);
    let mut definitions = HashSet::new();
    for saved in &self.task_definitions {
      if !valid_text(&saved.definition_id)
        || !valid_text(&saved.revision)
        || !valid_text(&saved.definition.name)
        || !valid_text(&saved.definition.program)
        || saved.definition.arguments.len() > 4096
        || saved
          .definition
          .arguments
          .iter()
          .any(|arg| arg.len() > 65536 || arg.contains('\0'))
        || saved
          .definition
          .working_directory
          .as_ref()
          .is_some_and(|cwd| !valid_text(cwd))
        || !definitions.insert(saved.definition_id.as_str())
      {
        return Err(invalid());
      }
    }
    let mut task_ids = HashSet::new();
    let mut defaults = HashSet::new();
    for task in &self.task_references {
      if !hosts.contains(task.host_id.as_str())
        || uuid::Uuid::parse_str(&task.task_id).is_err()
        || !task_ids.insert((task.host_id.as_str(), task.task_id.as_str()))
        || task
          .definition_id
          .as_ref()
          .is_some_and(|id| !definitions.contains(id.as_str()))
        || (task.is_default
          && task
            .definition_id
            .as_ref()
            .is_some_and(|id| !defaults.insert((task.host_id.as_str(), id.as_str()))))
      {
        return Err(invalid());
      }
    }
    let valid_tab = |tab: &WorkspaceTab| match tab {
      WorkspaceTab::Session {
        host_id,
        session_id,
      } => sessions.contains(&SessionReference {
        host_id: host_id.clone(),
        session_id: session_id.clone(),
      }),
      WorkspaceTab::Task { host_id, task_id } => {
        task_ids.contains(&(host_id.as_str(), task_id.as_str()))
      }
      WorkspaceTab::TaskDefinition { definition_id } => {
        definitions.contains(definition_id.as_str())
      }
    };

    if self.tabs.iter().any(|tab| !valid_tab(tab)) {
      return Err(invalid());
    }
    Ok(())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
  /// Opaque compare-and-swap revision, not a JavaScript numeric counter.
  pub revision: Option<String>,
  pub document: WorkspaceDocument,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkspaceRequest {
  pub expected_revision: Option<String>,
  pub document: WorkspaceDocument,
}

#[tauri::command]
pub async fn load_workspace(app: tauri::AppHandle) -> CommandResult<WorkspaceSnapshot> {
  let directory = app
    .path()
    .app_data_dir()
    .map_err(CommandErrorDto::backend)?;
  tauri::async_runtime::spawn_blocking(move || repository::Repository::new(directory).load())
    .await
    .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn update_workspace(
  app: tauri::AppHandle,
  request: UpdateWorkspaceRequest,
) -> CommandResult<WorkspaceSnapshot> {
  let directory = app
    .path()
    .app_data_dir()
    .map_err(CommandErrorDto::backend)?;
  tauri::async_runtime::spawn_blocking(move || {
    repository::Repository::new(directory).update(request)
  })
  .await
  .map_err(CommandErrorDto::backend)?
}
