use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{StoreError, io_error};

/// The local namespace in which definition names must be unique.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DefinitionScope {
  Global,
  Project { project_root: PathBuf },
}

impl DefinitionScope {
  /// Resolves this scope to its definition file.
  ///
  /// # Errors
  /// Returns an error if the global configuration directory is unavailable or
  /// the project root cannot be resolved to an existing directory.
  pub fn path(&self) -> Result<PathBuf, StoreError> {
    match self {
      Self::Global => global_path(),
      Self::Project { project_root } => project_path(project_root),
    }
  }
}

/// Returns the user's platform configuration directory followed by `ctl/tasks.json`.
///
/// # Errors
/// Returns an error when the platform has no configuration directory.
pub fn global_path() -> Result<PathBuf, StoreError> {
  dirs::config_dir()
    .map(|directory| directory.join("ctl/tasks.json"))
    .ok_or(StoreError::ConfigDirectoryUnavailable)
}

/// Returns `<project-root>/.ctl/tasks.json` using the root's physical absolute path.
///
/// # Errors
/// Returns an error if the root is missing, inaccessible, or is not a directory.
pub fn project_path(root: &Path) -> Result<PathBuf, StoreError> {
  Ok(project_directory(root)?.join(".ctl/tasks.json"))
}

/// Finds the nearest project marker while walking from an existing directory.
///
/// An existing `.ctl/tasks.json` takes precedence at each level. A `.git` file
/// or directory also marks a root, including Git worktrees. Discovery stops at
/// that root instead of falling through to a parent project's definitions.
///
/// # Errors
/// Returns errors for inaccessible paths instead of silently selecting another scope.
pub fn discover_project(start: &Path) -> Result<Option<PathBuf>, StoreError> {
  let start = project_directory(start)?;
  for ancestor in start.ancestors() {
    let store = ancestor.join(".ctl/tasks.json");
    match fs::symlink_metadata(&store) {
      Ok(_) => return Ok(Some(ancestor.to_path_buf())),
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(io_error(store, error)),
    }
    let git = ancestor.join(".git");
    match fs::metadata(&git) {
      Ok(metadata) if metadata.is_file() || metadata.is_dir() => {
        return Ok(Some(ancestor.to_path_buf()));
      }
      Ok(_) => {}
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(io_error(git, error)),
    }
  }
  Ok(None)
}

fn project_directory(root: &Path) -> Result<PathBuf, StoreError> {
  let path = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
  if !path.is_dir() {
    return Err(StoreError::InvalidProjectRoot { path });
  }
  Ok(path)
}
