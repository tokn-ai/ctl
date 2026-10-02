//! Atomic shared-helper selections, separate from the desktop release selection.

use super::CompatibleInstallation;
use super::filesystem::{
  check_directory, checked_component_directory, ensure_directory, invalid_path, owned,
  validate_candidate,
};
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Returns the compatibility selection's path without inspecting the filesystem.
///
/// # Errors
/// Rejects targets other than the two supported native macOS architectures.
pub fn compatible_selection(home: &Path, target: &str) -> io::Result<PathBuf> {
  Ok(
    super::component_directory(home)
      .join("selected")
      .join(selection_name(target)?),
  )
}

/// Checks that setup can safely replace the compatibility selection.
///
/// Missing selections and well-formed dangling links are allowed for repair.
/// This does not create directories or validate the selected bundle, and does
/// not make an executable safe to launch.
///
/// # Errors
/// Rejects unsupported targets, unsafe managed parents, and a selection that
/// is foreign-owned, is not a symlink, or escapes the immutable caches.
pub fn validate_compatible_selection(home: &Path, target: &str) -> io::Result<()> {
  let name = selection_name(target)?;
  let directory = match checked_component_directory(home) {
    Ok(directory) => directory,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  };
  let selected = directory.join("selected");
  match check_directory(&selected, true) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  }
  checked_selection(&selected.join(name)).map(|_| ())
}

/// Pins one shared installation matching the client's target and required APIs.
///
/// A missing selection or a removed installation returns `None`. An existing
/// incomplete or unsafe installation is an error, so discovery cannot bypass
/// a corrupted managed app by falling back to an unsigned executable.
///
/// # Errors
/// Rejects unsupported targets, unsafe parents, malformed or foreign links,
/// and incomplete or writable selected bundles.
pub fn resolve_compatible_installation(
  home: &Path,
  target: &str,
) -> io::Result<Option<CompatibleInstallation>> {
  let name = selection_name(target)?;
  let directory = match checked_component_directory(home) {
    Ok(directory) => directory,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let selected = directory.join("selected");
  match check_directory(&selected, true) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  }
  let Some((relative, development)) = checked_selection(&selected.join(name))? else {
    return Ok(None);
  };
  let candidate = directory.join(&relative);
  // Check the cache parent even if the immutable installation was removed.
  // This prevents a symlinked or writable cache from being treated as absent.
  check_directory(
    &directory.join(if development {
      "development"
    } else {
      "versions"
    }),
    true,
  )?;
  match fs::symlink_metadata(&candidate) {
    Ok(_) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  }
  validate_candidate(&directory, &relative, development)?;
  Ok(Some(CompatibleInstallation {
    directory: candidate,
    development,
  }))
}

/// Atomically selects an already verified immutable cached installation.
///
/// The caller must hold the component setup lock and must have verified the
/// candidate's signature, metadata, and required APIs before calling this.
/// This selection never changes the desktop release `current` link.
///
/// # Errors
/// Rejects unsupported targets, untrusted managed paths, staging directories,
/// incomplete candidates, and malformed or foreign existing selections.
pub fn select_compatible_installation(
  home: &Path,
  target: &str,
  candidate: &Path,
  development: bool,
) -> io::Result<()> {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  let name = selection_name(target)?;
  let directory = checked_component_directory(home)?;
  let relative = candidate
    .strip_prefix(&directory)
    .or_else(|_| candidate.strip_prefix(super::component_directory(home)))
    .map_err(|_| invalid_path(candidate, "must be inside the managed component directory"))?;
  let reference = Path::new("..").join(relative);
  let Some((installation, is_development)) = parse_reference(&reference) else {
    return Err(invalid_path(
      candidate,
      "must name an immutable cached installation",
    ));
  };
  if is_development != development {
    return Err(invalid_path(
      candidate,
      "must match the selected signing mode",
    ));
  }
  validate_candidate(&directory, &installation, development)?;
  let selected = directory.join("selected");
  ensure_directory(&selected, true)?;
  let selection = selected.join(name);
  checked_selection(&selection)?;
  let temporary = selected.join(format!(
    ".selection-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  ));
  // symlink fails if the temporary name already exists; it never follows or
  // replaces another entry while preparing the atomic rename.
  symlink(&reference, &temporary)?;
  let result = fs::rename(&temporary, &selection);
  if result.is_err() {
    let _ = fs::remove_file(&temporary);
  }
  result
}

fn selection_name(target: &str) -> io::Result<String> {
  if !matches!(target, "aarch64-apple-darwin" | "x86_64-apple-darwin") {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "shared ctld.app selections require a supported native macOS target",
    ));
  }
  Ok(format!(
    "{target}-ctld{}-lifecycle{}-helper{}",
    crate::PROTOCOL_VERSION,
    crate::lifecycle::PROTOCOL_VERSION,
    crate::HELPER_API_VERSION
  ))
}

fn checked_selection(path: &Path) -> io::Result<Option<(PathBuf, bool)>> {
  let metadata = match fs::symlink_metadata(path) {
    Ok(metadata) => metadata,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  if !metadata.file_type().is_symlink() || !owned(&metadata) {
    return Err(invalid_path(path, "must be a symlink owned by this user"));
  }
  let reference = fs::read_link(path)?;
  parse_reference(&reference).map(Some).ok_or_else(|| {
    invalid_path(
      path,
      "must point directly to ../versions/<id> or ../development/<sha256>",
    )
  })
}

fn parse_reference(path: &Path) -> Option<(PathBuf, bool)> {
  let components: Vec<_> = path.components().collect();
  if path.as_os_str() != components.iter().collect::<PathBuf>().as_os_str() {
    return None;
  }
  let [
    Component::ParentDir,
    Component::Normal(parent),
    Component::Normal(name),
  ] = components.as_slice()
  else {
    return None;
  };
  let development = if *parent == "versions" {
    false
  } else if *parent == "development"
    && name.to_str().is_some_and(|value| {
      value.len() == 64
        && value
          .bytes()
          .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
  {
    true
  } else {
    return None;
  };
  Some((Path::new(parent).join(name), development))
}
