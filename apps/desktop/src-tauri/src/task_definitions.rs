//! Shared local definition storage. Does not start taskd or execute commands.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use task_store::{DefinitionScope, Repository, SavedTaskDefinition};

use crate::error::{CommandErrorDto, CommandResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadDefinitionsRequest {
  scope: DefinitionScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveDefinitionRequest {
  scope: DefinitionScope,
  definition_id: String,
  expected_revision: Option<String>,
  definition: task_proto::TaskDefinition,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveDefinitionRequest {
  scope: DefinitionScope,
  definition_id: String,
  expected_revision: String,
}

#[derive(Serialize)]
pub struct DefinitionCatalog {
  scope: DefinitionScope,
  path: PathBuf,
  definitions: Vec<SavedTaskDefinition>,
}

fn scope_path(scope: &DefinitionScope) -> CommandResult<PathBuf> {
  if let DefinitionScope::Project { project_root } = scope
    && !project_root.is_absolute()
  {
    return Err(CommandErrorDto::new(
      "definition_scope_invalid",
      "Use an absolute project directory.",
    ));
  }
  scope.path().map_err(store_error)
}

// Owned adapter for Result::map_err.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn store_error(error: task_store::StoreError) -> CommandErrorDto {
  let code = match &error {
    task_store::StoreError::Conflict { .. } => "definition_conflict",
    task_store::StoreError::NameConflict { .. } => "definition_name_conflict",
    task_store::StoreError::NotFound { .. } => "definition_not_found",
    task_store::StoreError::InvalidProjectRoot { .. } => "definition_scope_invalid",
    _ => "definition_store_error",
  };
  CommandErrorDto::new(code, error.to_string())
}

#[tauri::command]
pub async fn load_task_definitions(
  request: LoadDefinitionsRequest,
) -> CommandResult<DefinitionCatalog> {
  tauri::async_runtime::spawn_blocking(move || {
    let path = scope_path(&request.scope)?;
    let scope = match request.scope {
      DefinitionScope::Global => DefinitionScope::Global,
      DefinitionScope::Project { .. } => DefinitionScope::Project {
        project_root: path
          .parent()
          .and_then(std::path::Path::parent)
          .expect("project_path ends in .ctl/tasks.json")
          .to_path_buf(),
      },
    };
    let snapshot = Repository::new(path.clone()).load().map_err(store_error)?;
    Ok(DefinitionCatalog {
      scope,
      path,
      definitions: snapshot.definitions,
    })
  })
  .await
  .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn save_task_definition(
  request: SaveDefinitionRequest,
) -> CommandResult<SavedTaskDefinition> {
  tauri::async_runtime::spawn_blocking(move || {
    Repository::new(scope_path(&request.scope)?)
      .save(
        &request.definition_id,
        request.expected_revision.as_deref(),
        request.definition,
      )
      .map_err(store_error)
  })
  .await
  .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn remove_task_definition(request: RemoveDefinitionRequest) -> CommandResult<()> {
  tauri::async_runtime::spawn_blocking(move || {
    Repository::new(scope_path(&request.scope)?)
      .remove(&request.definition_id, &request.expected_revision)
      .map_err(store_error)
  })
  .await
  .map_err(CommandErrorDto::backend)?
}
