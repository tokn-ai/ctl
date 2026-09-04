//! App-owned workspace metadata. Never connects to a daemon or stores runtime state.

#[cfg(all(test, unix))]
mod remote_test;
mod repository;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use task_store::DefinitionScope;
pub use task_store::SavedTaskDefinition;
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
pub struct TaskReference {
  pub host_id: String,
  pub task_id: String,
  pub definition_id: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub definition_scope: Option<DefinitionScope>,
  pub applied_revision: Option<String>,
  pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinitionDraft {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub command_line: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub scope: Option<DefinitionScope>,
  // Missing means a legacy draft with an unknown base; null means a new definition.
  #[serde(default, skip_serializing_if = "DraftBaseRevision::is_unknown")]
  pub base_revision: DraftBaseRevision,
  pub definition_id: String,
  pub definition: task_proto::TaskDefinition,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DraftBaseRevision {
  Saved(String),
  New,
  #[default]
  Unknown,
}

impl DraftBaseRevision {
  fn is_unknown(&self) -> bool {
    matches!(self, Self::Unknown)
  }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarView {
  #[default]
  Sessions,
  Tasks,
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
  #[serde(default = "global_definition_scope")]
  pub task_definition_scope: DefinitionScope,
  #[serde(default)]
  pub task_drafts: Vec<TaskDefinitionDraft>,
  #[serde(default)]
  pub sidebar_view: SidebarView,
  #[serde(default)]
  pub task_references: Vec<TaskReference>,
}

fn global_definition_scope() -> DefinitionScope {
  DefinitionScope::Global
}

impl Default for WorkspaceDocument {
  fn default() -> Self {
    Self {
      schema_version: 3,
      workspace_id: "default".into(),
      hosts: vec![WorkspaceHost {
        host_id: "local".into(),
        target: ConnectionTargetDto::Local,
      }],
      sessions: Vec::new(),
      task_definitions: Vec::new(),
      task_definition_scope: DefinitionScope::Global,
      task_drafts: Vec::new(),
      sidebar_view: SidebarView::default(),
      task_references: Vec::new(),
      tabs: Vec::new(),
      active_tab: None,
    }
  }
}

impl WorkspaceDocument {
  fn validate(&self) -> CommandResult<()> {
    if !matches!(self.schema_version, 2 | 3) {
      return Err(CommandErrorDto::new(
        "workspace_version_unsupported",
        "This workspace was written by another app version. Its file has not been changed.",
      ));
    }
    if self.schema_version == 3 && !self.task_definitions.is_empty() {
      return Err(CommandErrorDto::new(
        "workspace_invalid",
        "Saved task definitions belong in the shared definition store.",
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
    self.validate_task_drafts()?;
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
  fn validate_task_drafts(&self) -> CommandResult<()> {
    let mut ids = HashSet::new();
    let valid_field = |value: &str| value.len() <= 65536 && !value.contains('\0');
    if self.task_drafts.len() > 1024
      || self.task_drafts.iter().any(|draft| {
        draft.definition_id.is_empty()
          || draft.definition_id.len() > 4096
          || draft.definition_id.chars().any(char::is_control)
          || !ids.insert((
            &draft.definition_id,
            draft.scope.clone().unwrap_or(DefinitionScope::Global),
          ))
          || draft
            .command_line
            .as_ref()
            .is_some_and(|line| !valid_field(line))
          || !valid_field(&draft.definition.name)
          || !valid_field(&draft.definition.program)
          || draft.definition.arguments.len() > 4096
          || draft
            .definition
            .arguments
            .iter()
            .any(|arg| !valid_field(arg))
          || draft
            .definition
            .working_directory
            .as_ref()
            .is_some_and(|cwd| !valid_field(cwd))
      })
    {
      return Err(CommandErrorDto::new(
        "workspace_invalid",
        "Invalid task draft data.",
      ));
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
        || task.definition_id.as_ref().is_some_and(|id| {
          !valid_text(id) || (self.schema_version == 2 && !definitions.contains(id.as_str()))
        })
        || (task.is_default
          && task.definition_id.as_ref().is_some_and(|id| {
            let scope = task
              .definition_scope
              .clone()
              .unwrap_or(DefinitionScope::Global);
            !defaults.insert((task.host_id.as_str(), id.as_str(), scope))
          }))
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
        valid_text(definition_id)
          && (self.schema_version == 3 || definitions.contains(definition_id.as_str()))
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
  let definition_path = task_store::global_path().map_err(crate::task_definitions::store_error)?;
  tauri::async_runtime::spawn_blocking(move || {
    repository::Repository::new(directory)
      .with_definition_store(definition_path)
      .load()
  })
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
  let definition_path = task_store::global_path().map_err(crate::task_definitions::store_error)?;
  tauri::async_runtime::spawn_blocking(move || {
    repository::Repository::new(directory)
      .with_definition_store(definition_path)
      .update(request)
  })
  .await
  .map_err(CommandErrorDto::backend)?
}
