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

use ctl_task_store::DefinitionScope;
pub use ctl_task_store::SavedTaskDefinition;
use serde::{Deserialize, Serialize};

#[cfg(test)]
pub use catalog::HostCatalogDocument;
pub use catalog::{HostCatalogSnapshot, UpdateHostsRequest};
pub use hosts::{WorkspaceConnectionMethod, WorkspaceHost};

use crate::dto::{TerminalSizeDto, valid_observation_timestamp};
use crate::error::{CommandErrorDto, CommandResult};
pub use ctl_client::hosts::WorkspaceSshGateway;
use ctl_client::hosts::validated_gateway_ids;

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
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub last_known_terminal_size: Option<TerminalSizeDto>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub last_seen_at_ms: Option<u64>,
}

impl WorkspaceSession {
  fn invalid_metadata_field(&self) -> Option<&'static str> {
    for (field, value) in [
      ("session_id", Some(&self.session_id)),
      ("name", Some(&self.name)),
      ("last_known_cwd", self.last_known_cwd.as_ref()),
      (
        "last_known_cwd_display",
        self.last_known_cwd_display.as_ref(),
      ),
    ] {
      if value.is_some_and(|value| !valid_workspace_text(value)) {
        return Some(field);
      }
    }
    if self
      .last_known_terminal_size
      .as_ref()
      .is_some_and(|size| size.columns == 0 || size.rows == 0)
    {
      return Some("last_known_terminal_size");
    }
    if self
      .last_seen_at_ms
      .is_some_and(|value| !valid_observation_timestamp(value))
    {
      return Some("last_seen_at_ms");
    }
    None
  }

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
  pub definition: ctl_task_proto::TaskDefinition,
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
  Vpn,
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

fn invalid_workspace_field(field: &str) -> CommandErrorDto {
  CommandErrorDto::new(
    "workspace_invalid",
    format!("The workspace contains invalid or duplicate references in {field}."),
  )
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
    if !valid_workspace_text(&self.workspace_id) {
      return Err(invalid_workspace_field("workspace_id"));
    }
    if self.hosts.len() > 1024 {
      return Err(invalid_workspace_field("hosts: more than 1024 entries"));
    }
    if self.sessions.len() > 10_000 {
      return Err(invalid_workspace_field("sessions: more than 10000 entries"));
    }
    let hosts = if self.schema_version < 8 {
      if !self.host_identities.is_empty() {
        return Err(invalid_workspace_field(
          "host_identities: unsupported in legacy workspaces",
        ));
      }
      let gateway_ids = validated_gateway_ids(&self.ssh_gateways)
        .ok_or_else(|| invalid_workspace_field("ssh_gateways"))?;
      self
        .validated_host_ids(&gateway_ids)
        .ok_or_else(|| invalid_workspace_field("hosts"))?
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
        return Err(invalid_workspace_field("host_id"));
      }
      self.validate_host_identities()?;
      hosts
    };
    if !self.port_forwards_are_valid(&hosts) {
      return Err(invalid_workspace_field("port_forwards"));
    }
    let mut sessions = HashSet::new();
    for (index, session) in self.sessions.iter().enumerate() {
      let field = format!("sessions[{index}]");
      if !hosts.contains(session.host_id.as_str()) {
        return Err(invalid_workspace_field(&format!("{field}.host_id")));
      }
      if let Some(metadata_field) = session.invalid_metadata_field() {
        return Err(invalid_workspace_field(&format!(
          "{field}.{metadata_field}"
        )));
      }
      if !sessions.insert(session.reference()) {
        return Err(invalid_workspace_field(&format!(
          "{field}: duplicate session reference"
        )));
      }
    }
    self.validate_task_drafts()?;
    self.validate_task_tabs(&hosts, &sessions)?;
    let mut tabs = HashSet::new();
    for (index, tab) in self.tabs.iter().enumerate() {
      if !tabs.insert(tab) {
        return Err(invalid_workspace_field(&format!(
          "tabs[{index}]: duplicate"
        )));
      }
    }
    if self
      .active_tab
      .as_ref()
      .is_some_and(|tab| !tabs.contains(tab))
    {
      return Err(invalid_workspace_field("active_tab: absent from tabs"));
    }
    Ok(())
  }

  fn validate_host_identities(&self) -> CommandResult<()> {
    if self.host_identities.len() > 1024 {
      return Err(invalid_workspace_field(
        "host_identities: more than 1024 entries",
      ));
    }
    // Gateway-only hosts are referenced by routes in the separate catalog.
    // Keep their identity pins even without sessions, tasks, or forwards, and
    // without depending on the current catalog's availability.
    let mut identities = HashSet::new();
    for (index, item) in self.host_identities.iter().enumerate() {
      let field = format!("host_identities[{index}]");
      if item.host_id == "local" || !valid_workspace_text(&item.host_id) {
        return Err(invalid_workspace_field(&format!("{field}.host_id")));
      }
      if !identities.insert(item.host_id.as_str()) {
        return Err(invalid_workspace_field(&format!(
          "{field}.host_id: duplicate"
        )));
      }
      if !item.remote_info.is_valid() {
        return Err(invalid_workspace_field(&format!("{field}.remote_info")));
      }
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

fn workspace_repository(_app: &tauri::AppHandle) -> CommandResult<repository::Repository> {
  let directory = ctl_core::paths::directory().map_err(|_| {
    CommandErrorDto::new(
      "home_directory_unavailable",
      "Could not find the home directory for the workspace.",
    )
  })?;
  let definition_path =
    ctl_task_store::global_path().map_err(crate::task_definitions::store_error)?;
  Ok(repository::Repository::new(directory).with_definition_store(definition_path))
}
