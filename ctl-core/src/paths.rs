//! Shared per-user storage for ctl and ctmux, independent of desktop app IDs.

use std::{
  env, io,
  path::{Component, Path, PathBuf},
};

/// Resolves the local state directory without creating it.
///
/// # Errors
/// Returns an error when no override is set and the home directory is unavailable.
pub fn directory() -> io::Result<PathBuf> {
  if let Some(directory) = data_override() {
    return Ok(directory);
  }
  dirs::home_dir()
    .map(|home| home.join(".tokn/ctl"))
    .ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "Could not locate the home directory.",
      )
    })
}

/// Resolves the configuration directory, falling back to the state directory.
///
/// # Errors
/// Returns an error when neither override nor the home directory is available.
pub fn config_directory() -> io::Result<PathBuf> {
  match config_override() {
    Some(directory) => Ok(directory),
    None => directory(),
  }
}

/// Returns the explicit configuration directory, if set.
#[must_use]
pub fn config_override() -> Option<PathBuf> {
  directory_override("CTL_CONFIG_DIR")
}

/// Returns the explicit local state directory, if set.
#[must_use]
pub fn data_override() -> Option<PathBuf> {
  directory_override("CTL_DATA_DIR")
}

fn directory_override(variable: &str) -> Option<PathBuf> {
  env::var_os(variable)
    .filter(|value| !value.is_empty())
    .map(PathBuf::from)
}

/// Resolves the storage root for a local account.
#[must_use]
pub fn directory_for_account(account: &Path) -> PathBuf {
  data_override().unwrap_or_else(|| account.join(".tokn/ctl"))
}

/// Identifies the installation directory that must be checked before walking
/// a managed storage path.
///
/// # Errors
/// Rejects paths outside the selected root or paths with parent traversals.
pub fn managed_storage_anchor(path: &Path) -> io::Result<&Path> {
  if path.components().any(|part| part == Component::ParentDir) {
    return Err(io::Error::other("invalid ctl storage path"));
  }
  if let Some(root) = data_override()
    && path.starts_with(&root)
    && root.is_absolute()
  {
    return path
      .ancestors()
      .nth(path.components().count() - root.components().count())
      .ok_or_else(|| io::Error::other("invalid alternate ctl storage path"));
  }
  path
    .ancestors()
    .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".tokn"))
    .and_then(Path::parent)
    .ok_or_else(|| io::Error::other("ctl storage is outside the selected directory"))
}

/// Rust target of these compiled components, including the Linux ABI.
#[must_use]
pub fn native_target() -> &'static str {
  env!("COMPONENT_TARGET")
}
