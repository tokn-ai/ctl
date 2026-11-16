//! Cargo provenance for checkout-local signed helpers, without runtime cwd discovery.

use sha2::{Digest as _, Sha256};
use std::io;
use std::path::{Path, PathBuf};

/// Canonical checkout identity and candidates in the Cargo target directory.
pub struct Context {
  pub repository_root: PathBuf,
  pub checkpoints: Vec<PathBuf>,
}

/// Captures checkout provenance for one repository package at Cargo build time.
///
/// # Errors
/// Returns filesystem errors while resolving the package and output paths.
pub fn context(
  manifest: &Path,
  repository_package: &Path,
  output: &Path,
  target: &str,
) -> io::Result<Option<Context>> {
  if !target.ends_with("-apple-darwin") {
    return Ok(None);
  }
  let Some(root) = manifest
    .ancestors()
    .nth(repository_package.components().count())
  else {
    return Ok(None);
  };
  let repository_root = root.canonicalize()?;
  // Packaged crates are no longer in the repository package layout. They retain
  // ordinary shared discovery rather than borrowing unrelated registry paths.
  if !repository_root.join("Cargo.toml").is_file()
    || repository_root
      .join(repository_package)
      .join("Cargo.toml")
      .canonicalize()
      .ok()
      != Some(manifest.join("Cargo.toml").canonicalize()?)
  {
    return Ok(None);
  }
  let Some(repository_name) = repository_root.to_str() else {
    return Ok(None);
  };
  let identity = format!("{:x}", Sha256::digest(repository_name.as_bytes()));
  let output = output.canonicalize()?;
  let Some(profile) = output
    .parent()
    .and_then(Path::parent)
    .and_then(Path::parent)
  else {
    return Ok(None);
  };
  let Some(target_directory) = profile.parent() else {
    return Ok(None);
  };
  let mut roots = vec![target_directory.to_path_buf()];
  // With an explicit --target, Cargo inserts the triple between its target
  // directory and profile. Keep both layouts without inferring from runtime cwd.
  if target_directory
    .file_name()
    .is_some_and(|name| name == target)
    && let Some(parent) = target_directory.parent()
  {
    roots.push(parent.to_path_buf());
  }
  let checkpoints: Vec<_> = roots
    .into_iter()
    .map(|root| root.join("ctl-dev/helpers").join(&identity[..20]))
    .collect();
  if checkpoints.iter().any(|path| path.to_str().is_none()) {
    return Ok(None);
  }
  Ok(Some(Context {
    repository_root,
    checkpoints,
  }))
}

/// Writes debug-only runtime provenance shared by CLI and GUI consumers.
///
/// # Errors
/// Rejects non-UTF-8 paths and returns errors writing the generated Rust source.
pub fn write(output: &Path, context: Option<&Context>) -> io::Result<()> {
  let invalid = || {
    io::Error::new(
      io::ErrorKind::InvalidInput,
      "Development helper paths must be UTF-8",
    )
  };
  let selected = match context {
    None => "None".to_owned(),
    Some(context) => {
      let repository = context.repository_root.to_str().ok_or_else(invalid)?;
      let checkpoints = context
        .checkpoints
        .iter()
        .map(|path| {
          path
            .to_str()
            .map(|value| format!("{value:?}"))
            .ok_or_else(invalid)
        })
        .collect::<io::Result<Vec<_>>>()?;
      format!(
        "Some(ctl_client::setup::DevelopmentContext {{ repository_root: {repository:?}, checkpoints: &[{}] }})",
        checkpoints.join(", "),
      )
    }
  };
  // Compile the provenance only into ordinary debug macOS executables. Product
  // releases and all embedded payload builds keep their existing discovery.
  std::fs::write(
    output.join("development_ctld.rs"),
    format!(
      "#[cfg(all(debug_assertions, target_os = \"macos\"))]\nconst LOCAL_DEVELOPMENT: Option<ctl_client::setup::DevelopmentContext> = {selected};\n#[cfg(not(all(debug_assertions, target_os = \"macos\")))]\nconst LOCAL_DEVELOPMENT: Option<ctl_client::setup::DevelopmentContext> = None;\n",
    ),
  )
}
