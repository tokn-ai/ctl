//! App-owned workspace metadata. Never connects to a daemon or stores runtime state.

mod catalog;
#[cfg(test)]
mod catalog_tests;
mod hosts;
#[cfg(test)]
mod location_tests;
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

#[cfg(test)]
pub use catalog::HostCatalogDocument;
pub use catalog::{HostCatalogSnapshot, UpdateHostsRequest};
pub use hosts::{WorkspaceConnectionMethod, WorkspaceHost};

fn is_ssh_gateway_kind(kind: &ctld_ipc::GatewayKind) -> bool {
  *kind == ctld_ipc::GatewayKind::Ssh
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSshGateway {
  #[serde(default, skip_serializing_if = "is_ssh_gateway_kind")]
  pub kind: ctld_ipc::GatewayKind,
  pub gateway_id: String,
  pub name: String,
  pub destination: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hostname: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub user: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub port: Option<u16>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub identity_file: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub remote_info: Option<ctl_proto::RemoteIdentity>,
}
use crate::error::{CommandErrorDto, CommandResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePortForward {
  pub forward_id: String,
  pub host_id: String,
  pub name: String,
  pub bind_address: String,
  pub local_port: u16,
  pub remote_host: String,
  pub remote_port: u16,
  pub enabled: bool,
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
  Ports,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDocument {
  pub schema_version: u32,
  pub workspace_id: String,
  /// Legacy input only. Version 8 stores reusable connections in hosts.json.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub hosts: Vec<WorkspaceHost>,
  #[serde(default)]
  pub host_identities: Vec<WorkspaceHostIdentity>,
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
  #[serde(default)]
  pub port_forwards: Vec<WorkspacePortForward>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub ssh_gateways: Vec<WorkspaceSshGateway>,
}

/// Remembered identity of a referenced environment, never connection settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHostIdentity {
  pub host_id: String,
  pub remote_info: ctl_proto::RemoteIdentity,
}

fn global_definition_scope() -> DefinitionScope {
  DefinitionScope::Global
}

fn valid_workspace_text(text: &str) -> bool {
  !text.is_empty() && text.len() <= 4096 && !text.chars().any(char::is_control)
}

impl Default for WorkspaceDocument {
  fn default() -> Self {
    Self {
      schema_version: 8,
      workspace_id: "default".into(),
      hosts: Vec::new(),
      host_identities: Vec::new(),
      sessions: Vec::new(),
      task_definitions: Vec::new(),
      task_definition_scope: DefinitionScope::Global,
      task_drafts: Vec::new(),
      sidebar_view: SidebarView::default(),
      task_references: Vec::new(),
      port_forwards: Vec::new(),
      ssh_gateways: Vec::new(),
      tabs: Vec::new(),
      active_tab: None,
    }
  }
}

fn validated_gateway_ids(gateways: &[WorkspaceSshGateway]) -> Option<HashSet<&str>> {
  let mut gateway_ids = HashSet::new();
  let invalid = gateways.len() > 256
    || gateways.iter().any(|gateway| {
      !gateway_ids.insert(gateway.gateway_id.as_str())
        || !valid_workspace_text(&gateway.gateway_id)
        || !valid_workspace_text(&gateway.name)
        || !valid_workspace_text(&gateway.destination)
        || gateway
          .destination
          .chars()
          .any(|value| matches!(value, ',' | '@'))
        || gateway.port == Some(0)
        || (gateway.kind == ctld_ipc::GatewayKind::Socks5
          && (gateway.port.is_none()
            || gateway.hostname.is_some()
            || gateway.identity_file.is_some()
            || gateway.remote_info.is_some()))
        || gateway
          .remote_info
          .as_ref()
          .is_some_and(|info| !info.is_valid())
        || [
          gateway.hostname.as_ref(),
          gateway.user.as_ref(),
          gateway.identity_file.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_workspace_text(value))
        || gateway.hostname.as_ref().is_some_and(|value| {
          value.chars().any(|value| matches!(value, ',' | '@'))
            || value.chars().any(char::is_whitespace)
        })
        || gateway.user.as_ref().is_some_and(|value| {
          value.chars().any(|value| matches!(value, ',' | '@'))
            || value.chars().any(char::is_whitespace)
        })
    });
  (!invalid).then_some(gateway_ids)
}

impl WorkspaceDocument {
  fn validated_host_ids<'a>(&'a self, gateway_ids: &HashSet<&str>) -> Option<HashSet<&'a str>> {
    let mut hosts = HashSet::new();
    for host in &self.hosts {
      if !hosts.insert(host.host_id.as_str()) || !host.is_valid(gateway_ids) {
        return None;
      }
    }
    hosts.contains("local").then_some(hosts)
  }

  fn validate(&self) -> CommandResult<()> {
    if !matches!(self.schema_version, 2..=8) {
      return Err(CommandErrorDto::new(
        "workspace_version_unsupported",
        "This workspace was written by another app version. Its file has not been changed.",
      ));
    }
    if self.schema_version >= 3 && !self.task_definitions.is_empty() {
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
    if !valid_workspace_text(&self.workspace_id)
      || self.hosts.len() > 1024
      || self.sessions.len() > 10_000
    {
      return Err(invalid());
    }
    let hosts = if self.schema_version < 8 {
      if !self.host_identities.is_empty() {
        return Err(invalid());
      }
      let gateway_ids = validated_gateway_ids(&self.ssh_gateways).ok_or_else(invalid)?;
      self.validated_host_ids(&gateway_ids).ok_or_else(invalid)?
    } else {
      if !self.hosts.is_empty() || !self.ssh_gateways.is_empty() {
        return Err(CommandErrorDto::new(
          "workspace_invalid",
          "Saved hosts and gateways belong in hosts.json.",
        ));
      }
      // A removed SSH alias or separately edited catalog must not destroy
      // workspace references. Resolve availability when connecting, not saving.
      let hosts: HashSet<&str> = self
        .sessions
        .iter()
        .map(|item| item.host_id.as_str())
        .chain(
          self
            .task_references
            .iter()
            .map(|item| item.host_id.as_str()),
        )
        .chain(self.port_forwards.iter().map(|item| item.host_id.as_str()))
        .chain(std::iter::once("local"))
        .collect();
      if hosts.iter().any(|host_id| !valid_workspace_text(host_id)) {
        return Err(invalid());
      }
      let mut identities = HashSet::new();
      if self.host_identities.len() > 1024
        || self.host_identities.iter().any(|item| {
          item.host_id == "local"
            || !hosts.contains(item.host_id.as_str())
            || !identities.insert(item.host_id.as_str())
            || !item.remote_info.is_valid()
        })
      {
        return Err(invalid());
      }
      hosts
    };
    if !self.port_forwards_are_valid(&hosts) {
      return Err(invalid());
    }
    let mut sessions = HashSet::new();
    for session in &self.sessions {
      if !hosts.contains(session.host_id.as_str())
        || !valid_workspace_text(&session.session_id)
        || !valid_workspace_text(&session.name)
        || [
          session.last_known_cwd.as_ref(),
          session.last_known_cwd_display.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !valid_workspace_text(value))
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

  fn port_forwards_are_valid(&self, hosts: &HashSet<&str>) -> bool {
    let mut forward_ids = HashSet::new();
    self.port_forwards.len() <= 4096
      && self.port_forwards.iter().all(|forward| {
        forward_ids.insert(forward.forward_id.as_str())
          && valid_workspace_text(&forward.forward_id)
          && valid_workspace_text(&forward.name)
          && forward.name.len() <= 128
          && forward.bind_address == "127.0.0.1"
          && forward.local_port != 0
          && forward.remote_port != 0
          && valid_workspace_text(&forward.remote_host)
          && !forward.remote_host.chars().any(char::is_whitespace)
          && forward.host_id != "local"
          && hosts.contains(forward.host_id.as_str())
      })
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
          && (self.schema_version >= 3 || definitions.contains(definition_id.as_str()))
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
  let repository = workspace_repository(&app)?;
  tauri::async_runtime::spawn_blocking(move || repository.load())
    .await
    .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn update_workspace(
  app: tauri::AppHandle,
  request: UpdateWorkspaceRequest,
) -> CommandResult<WorkspaceSnapshot> {
  let repository = workspace_repository(&app)?;
  tauri::async_runtime::spawn_blocking(move || repository.update(request))
    .await
    .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn load_hosts(app: tauri::AppHandle) -> CommandResult<HostCatalogSnapshot> {
  let repository = workspace_repository(&app)?;
  tauri::async_runtime::spawn_blocking(move || repository.load_hosts())
    .await
    .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn update_hosts(
  app: tauri::AppHandle,
  request: UpdateHostsRequest,
) -> CommandResult<HostCatalogSnapshot> {
  let repository = workspace_repository(&app)?;
  tauri::async_runtime::spawn_blocking(move || repository.update_hosts(request))
    .await
    .map_err(CommandErrorDto::backend)?
}

fn workspace_repository(app: &tauri::AppHandle) -> CommandResult<repository::Repository> {
  let home = dirs::home_dir().ok_or_else(|| {
    CommandErrorDto::new(
      "home_directory_unavailable",
      "Could not find the home directory for the workspace.",
    )
  })?;
  let legacy_directory = app
    .path()
    .app_data_dir()
    .map_err(CommandErrorDto::backend)?;
  let definition_path = task_store::global_path().map_err(crate::task_definitions::store_error)?;
  Ok(
    repository::Repository::new(home.join(".tokn").join("rmux"))
      .with_legacy_directory(legacy_directory)
      .with_definition_store(definition_path),
  )
}
