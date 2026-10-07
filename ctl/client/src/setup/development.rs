//! Checkout-scoped discovery of explicitly provisioned development helpers.

use super::{Error, manifest::Manifest};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

const MAX_SELECTION_BYTES: usize = 16 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 512;
const MAX_PACKAGE_DEPTH: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
  schema_version: u16,
  repository_root: String,
  target: String,
  directory: String,
}

struct Candidate {
  directory: PathBuf,
  receipt: Vec<u8>,
}

#[cfg(target_os = "macos")]
pub(super) async fn discover(
  checkpoint: &Path,
  repository_root: &Path,
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Option<PathBuf>, Error> {
  let target = super::macos::release_target()?;
  let Some(candidate) = load_candidate(checkpoint, repository_root, target)? else {
    return Ok(None);
  };
  let home = dirs::home_dir().ok_or(Error::HomeDirectory)?;
  verify_candidate(&home, &candidate, required).await
}

#[cfg(target_os = "macos")]
async fn verify_candidate(
  home: &Path,
  candidate: &Candidate,
  required: Option<ctl_core::protocol::ProtocolVersion>,
) -> Result<Option<PathBuf>, Error> {
  let info = super::inspect_ctld_package(home, &candidate.directory, &candidate.receipt).await?;
  if !super::discovery::compatible_for_helper_contract(&info, required) {
    return Ok(None);
  }
  // Never resolve selected.json again: the returned executable remains pinned
  // to the verified immutable build if another provision run changes selection.
  Ok(Some(
    candidate.directory.join("ctld.app/Contents/MacOS/ctld"),
  ))
}

fn load_candidate(
  checkpoint: &Path,
  repository_root: &Path,
  target: &str,
) -> Result<Option<Candidate>, Error> {
  let repository = validate_checkpoint_path(checkpoint, repository_root)?;
  let Some(metadata) = optional_metadata(checkpoint)? else {
    return Ok(None);
  };
  check_directory_metadata(&metadata, checkpoint, true)?;
  // Normal Cargo target directories may be 0755. Private helper state must be
  // 0700, and none of these cache directories may itself redirect via a link.
  for (depth, directory) in checkpoint.ancestors().skip(1).take(3).enumerate() {
    check_directory(directory, depth < 2)?;
  }
  let selected = checkpoint.join("selected.json");
  let Some(bytes) = read_optional(&selected, MAX_SELECTION_BYTES)? else {
    return Ok(None);
  };
  let selection: Selection = serde_json::from_slice(&bytes)
    .map_err(|_| invalid("selected.json is not a valid bounded development selection"))?;
  if selection.schema_version != 1 {
    return Err(invalid("selected.json has an unsupported schema version"));
  }
  if selection.repository_root != repository {
    return Err(invalid(
      "selected.json belongs to a different repository or worktree",
    ));
  }
  if selection.target != target {
    return Err(invalid(
      "selected.json belongs to a different machine architecture",
    ));
  }
  let sha256 = selection
    .directory
    .strip_prefix("build-")
    .filter(|digest| {
      digest.len() == 64
        && digest
          .bytes()
          .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
    .ok_or_else(|| invalid("selected.json contains an unsafe immutable build directory"))?;
  let directory = checkpoint.join(&selection.directory);
  check_directory(&directory, true)?;
  let receipt = read_optional(
    &directory.join("ctld-package.json"),
    super::manifest::MAX_MANIFEST_BYTES,
  )?
  .ok_or_else(|| invalid("selected build lacks its ctld-package.json receipt"))?;
  let manifest = Manifest::parse_installed(&receipt, target)
    .map_err(|error| invalid(format!("invalid development helper receipt: {error}")))?;
  if manifest.development.is_none() {
    return Err(invalid(
      "selected checkout helper requires a development signing receipt",
    ));
  }
  if manifest.sha256 != sha256 {
    return Err(invalid(
      "selected build directory does not match its receipt archive checksum",
    ));
  }
  check_package(&directory.join("ctld.app"))?;
  Ok(Some(Candidate {
    directory: directory.canonicalize()?,
    receipt,
  }))
}

fn validate_checkpoint_path<'a>(
  checkpoint: &Path,
  repository_root: &'a Path,
) -> Result<&'a str, Error> {
  // The caller supplies canonical build provenance. Discovery must keep
  // working when that source checkout has since been removed or relocated.
  if !repository_root.is_absolute()
    || repository_root
      .components()
      .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    || repository_root.as_os_str()
      != repository_root
        .components()
        .collect::<PathBuf>()
        .as_os_str()
  {
    return Err(invalid(
      "repository build provenance must be an absolute normalized path",
    ));
  }
  let repository = repository_root.to_str().ok_or_else(|| {
    invalid("repository path is not UTF-8 and cannot identify its development helper")
  })?;
  let digest = format!("{:x}", Sha256::digest(repository.as_bytes()));
  let helpers = checkpoint
    .parent()
    .ok_or_else(|| invalid("checkpoint has no helper cache parent"))?;
  let development = helpers
    .parent()
    .ok_or_else(|| invalid("checkpoint has no development cache parent"))?;
  development
    .parent()
    .ok_or_else(|| invalid("checkpoint has no Cargo target directory"))?;
  if !checkpoint.is_absolute()
    || checkpoint
      .components()
      .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    || checkpoint.as_os_str() != checkpoint.components().collect::<PathBuf>().as_os_str()
    || checkpoint.file_name().and_then(|name| name.to_str()) != Some(&digest[..20])
    || helpers.file_name() != Some("helpers".as_ref())
    || development.file_name() != Some("ctl-dev".as_ref())
  {
    return Err(invalid(
      "checkpoint path does not identify this repository's ctl-dev/helpers cache",
    ));
  }
  Ok(repository)
}

fn invalid(message: impl Into<String>) -> Error {
  Error::Verification(format!("checkout development helper: {}", message.into()))
}

fn optional_metadata(path: &Path) -> Result<Option<fs::Metadata>, Error> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => Ok(Some(metadata)),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error.into()),
  }
}

fn check_directory(path: &Path, private: bool) -> Result<(), Error> {
  check_directory_metadata(&fs::symlink_metadata(path)?, path, private)
}

fn check_directory_metadata(
  metadata: &fs::Metadata,
  path: &Path,
  private: bool,
) -> Result<(), Error> {
  if !metadata.is_dir()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o022 != 0
    || (private && metadata.mode() & 0o777 != 0o700)
  {
    return Err(invalid(format!(
      "{} must be an owned {}directory without symlinks or group/other write access",
      path.display(),
      if private { "private " } else { "" }
    )));
  }
  Ok(())
}

fn read_optional(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, Error> {
  let descriptor = match rustix::fs::open(
    path,
    rustix::fs::OFlags::RDONLY
      | rustix::fs::OFlags::NOFOLLOW
      | rustix::fs::OFlags::NONBLOCK
      | rustix::fs::OFlags::CLOEXEC,
    rustix::fs::Mode::empty(),
  ) {
    Ok(descriptor) => descriptor,
    Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
    Err(error) => {
      return Err(invalid(format!(
        "could not open {} as a regular owned file: {}",
        path.display(),
        io::Error::from(error)
      )));
    }
  };
  let file = File::from(descriptor);
  check_file_metadata(&file.metadata()?, path)?;
  let mut bytes = Vec::new();
  file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
  if bytes.len() > limit {
    return Err(invalid(format!(
      "{} exceeds its size limit",
      path.display()
    )));
  }
  Ok(Some(bytes))
}

fn check_file_metadata(metadata: &fs::Metadata, path: &Path) -> Result<(), Error> {
  if !metadata.is_file()
    || metadata.uid() != rustix::process::getuid().as_raw()
    || metadata.mode() & 0o022 != 0
  {
    return Err(invalid(format!(
      "{} must be an owned regular file without symlinks or group/other write access",
      path.display()
    )));
  }
  Ok(())
}

fn check_package(app: &Path) -> Result<(), Error> {
  check_directory(app, false)?;
  let mut pending = vec![(app.to_owned(), 0_usize)];
  let mut count = 0;
  while let Some((path, depth)) = pending.pop() {
    count += 1;
    if count > MAX_PACKAGE_ENTRIES || depth > MAX_PACKAGE_DEPTH {
      return Err(invalid(
        "signed helper package contains too many or deeply nested entries",
      ));
    }
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_dir() {
      check_directory_metadata(&metadata, &path, false)?;
      for entry in fs::read_dir(&path)? {
        pending.push((entry?.path(), depth + 1));
        if pending.len() > MAX_PACKAGE_ENTRIES {
          return Err(invalid("signed helper package contains too many entries"));
        }
      }
    } else {
      check_file_metadata(&metadata, &path)?;
    }
  }
  for name in [
    "Contents/Info.plist",
    "Contents/embedded.provisionprofile",
    "Contents/_CodeSignature/CodeResources",
    "Contents/MacOS/ctld",
  ] {
    let path = app.join(name);
    check_file_metadata(&fs::symlink_metadata(&path)?, &path)?;
  }
  if fs::symlink_metadata(app.join("Contents/MacOS/ctld"))?.mode() & 0o100 == 0 {
    return Err(invalid(
      "signed helper executable is not executable by its owner",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests;
