//! Shared local storage for reusable task definitions, independent of taskd.

mod paths;
mod repository;

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use task_proto::TaskDefinition;
use thiserror::Error;

pub use paths::{DefinitionScope, discover_project, global_path, project_path};
pub use repository::Repository;

/// A reusable definition. Its revision is derived from its command values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedTaskDefinition {
  pub definition_id: String,
  #[serde(default)]
  pub revision: String,
  pub definition: TaskDefinition,
}

/// Definitions in one project or global store, sorted by name and identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
  pub definitions: Vec<SavedTaskDefinition>,
}

#[derive(Debug, Error)]
pub enum StoreError {
  #[error("could not access task definitions at {}: {source}", path.display())]
  Io {
    path: PathBuf,
    #[source]
    source: io::Error,
  },
  #[error("invalid task-definition JSON at {}: {source}", path.display())]
  Json {
    path: PathBuf,
    #[source]
    source: serde_json::Error,
  },
  #[error("invalid task definition: {message}")]
  InvalidDefinition { message: String },
  #[error("task-definition store version {version} is not supported; the file was preserved")]
  UnsupportedVersion { version: u64 },
  #[error("the task-definition store exceeds its 4 MiB size limit; the file was preserved")]
  TooLarge,
  #[error("task-definition paths must be regular files, not symlinks or directories: {}", path.display())]
  UnsafePath { path: PathBuf },
  #[error("definition {definition_id:?} changed; reload it before saving")]
  Conflict { definition_id: String },
  #[error("a definition named {name:?} already exists in this scope")]
  NameConflict { name: String },
  #[error("definition {definition_id:?} was not found")]
  NotFound { definition_id: String },
  #[error("the user's configuration directory is unavailable")]
  ConfigDirectoryUnavailable,
  #[error("project root must be an existing directory: {}", path.display())]
  InvalidProjectRoot { path: PathBuf },
}

fn io_error(path: impl Into<PathBuf>, source: io::Error) -> StoreError {
  StoreError::Io {
    path: path.into(),
    source,
  }
}
