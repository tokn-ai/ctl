//! A separate store for ctream previews and their matching remote agents.

use super::{
  AGENT_DIRECTORY, AgentTarget, BuildMetadata, MAX_AGENT_BYTES, MAX_ARCHIVE_BYTES,
  MAX_PAYLOAD_BYTES, agent_paths, archive_name, native_target, viewer_executables,
};
use ctl_core::{installations::CatalogOrigin, installed_build_name, paths, private_fs};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Cursor, Read as _, Write as _};
use std::path::{Path, PathBuf};

const DEVELOPMENT_RECEIPT: &str = ".development.json";
const CONTENT_ID_FILE: &str = ".content-id";
const CONTENT_ID_BYTES: usize = 64;
const MAX_RECEIPT_BYTES: u64 = 4096;

/// An installed ctream generation and its launcher paths.
#[derive(Debug, Serialize)]
pub struct Installation {
  /// Content-based identity of the generation.
  pub id: String,
  /// Directory name assigned to the installed build.
  pub build_id: String,
  /// Whether the generation was built locally from a checkout.
  pub development: bool,
  /// Directory containing the generation and bundled agents.
  pub directory: PathBuf,
  /// Installed preview executable.
  pub preview: PathBuf,
  /// Installed command-line executable.
  pub cli: PathBuf,
}

/// Resolve ctream's installation root for an account.
#[must_use]
pub fn root_for_account(home: &Path) -> PathBuf {
  paths::updater_directory_for_account(home).join("store/ctream")
}

/// Read the active ctream generation, if one is selected.
///
/// # Errors
/// Returns an error if the store or its selection cannot be verified.
pub fn active(root: &Path) -> io::Result<Option<Installation>> {
  if !storage(root, false)? {
    return Ok(None);
  }
  let Some(build_id) = read_current(root)? else {
    return Ok(None);
  };
  let path = root.join(&build_id);
  let id = installed_content_id(&path, &build_id)?;
  installed(&path, &build_id, id).map(Some)
}

/// Find the currently selected preview in the same store as a running managed preview.
/// # Errors
/// Returns an error if the store or its selection cannot be verified.
pub fn active_managed_preview(executable: &Path) -> io::Result<Option<PathBuf>> {
  let Some(generation) = executable.parent() else {
    return Ok(None);
  };
  let Some(root) = generation
    .parent()
    .filter(|root| root.ends_with("ctlup/store/ctream"))
  else {
    return Ok(None);
  };
  if !generation
    .file_name()
    .and_then(|name| name.to_str())
    .is_some_and(valid_build_id)
    || executable.file_name() != Some(binary_names()[0].as_ref())
  {
    return Ok(None);
  }
  Ok(active(root)?.map(|installation| installation.preview))
}

/// Verify a release ZIP, install its payload, and select the generation.
///
/// # Errors
/// Returns an error if the archive is invalid or the generation cannot be stored or selected.
pub fn install(
  root: &Path,
  archive: &Path,
  workflow_build: Option<&BuildMetadata>,
) -> io::Result<Installation> {
  let target = native_target()?;
  let bytes = archive_bytes(archive)?;
  let id = hex::encode(Sha256::digest(&bytes));
  let mut bundle = zip::ZipArchive::new(Cursor::new(bytes)).map_err(invalid)?;
  let embedded_build = BuildMetadata::from_archive_comment(bundle.comment())?;
  let label = build_label(embedded_build.as_ref().or(workflow_build), &id);

  let store = Store::open(root)?;
  let installation = match store.slot(&label, &id)? {
    BuildSlot::Installed(installation) => installation,
    BuildSlot::Vacant(build_id) => {
      let staging = store.stage()?;
      extract(&mut bundle, target, &staging.0)?;
      store.publish(&staging, &build_id, id)?
    }
  };
  store.select(installation)
}

/// Snapshot development binaries into a generation and select it.
///
/// `inputs` maps relative bundle paths to the binaries produced by the build.
///
/// # Errors
/// Returns an error if the inputs are invalid or the generation cannot be stored or selected.
pub fn install_development(
  root: &Path,
  inputs: &BTreeMap<String, PathBuf>,
  build: &BuildMetadata,
) -> io::Result<Installation> {
  DevelopmentReceipt::validate_names(inputs)?;
  let mut receipt = DevelopmentReceipt {
    target: native_target()?.into(),
    files: BTreeMap::new(),
  };
  let store = Store::open(root)?;
  let staging = store.stage()?;
  let mut remaining = MAX_PAYLOAD_BYTES;
  for (name, path) in inputs {
    let (size, digest) = snapshot_file(path, &staging.0.join(name), remaining)?;
    remaining -= size;
    receipt.files.insert(name.clone(), digest);
  }
  let id = receipt.id();
  let label = build_label(Some(build), &id);
  let installation = match store.slot(&label, &id)? {
    BuildSlot::Installed(installation) => installation,
    BuildSlot::Vacant(build_id) => {
      let mut file = new_file(&staging.0.join(DEVELOPMENT_RECEIPT))?;
      serde_json::to_writer(&mut file, &receipt).map_err(io::Error::other)?;
      file.sync_all()?;
      drop(file);
      store.publish(&staging, &build_id, id)?
    }
  };
  store.select(installation)
}

fn build_label(build: Option<&BuildMetadata>, id: &str) -> String {
  let origin = CatalogOrigin {
    channel: build.map_or("provided", |build| &build.channel).into(),
    built_at: build.and_then(|build| build.built_at.clone()),
    publication_id: None,
  };
  installed_build_name::label_from_version(
    build
      .and_then(|build| build.version.as_deref())
      .unwrap_or("unknown"),
    build.and_then(|build| build.revision.as_deref()),
    Some(&origin),
    id,
  )
}

enum BuildSlot {
  Installed(Installation),
  Vacant(String),
}

struct Store<'a> {
  root: &'a Path,
  current: Option<String>,
  _lock: File,
}

impl<'a> Store<'a> {
  fn open(root: &'a Path) -> io::Result<Self> {
    storage(root, true)?;
    let lock = lock(root)?;
    let current = read_current(root)?;
    Ok(Self {
      root,
      current,
      _lock: lock,
    })
  }

  fn stage(&self) -> io::Result<Temporary> {
    let path = self.root.join(format!(".partial-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path)?;
    let temporary = Temporary(path);
    for name in directories().skip(1) {
      private_fs::create_directory(&temporary.0.join(name))?;
    }
    Ok(temporary)
  }

  fn slot(&self, label: &str, id: &str) -> io::Result<BuildSlot> {
    for build_id in [
      label.to_owned(),
      format!("{label}-{}", &id[..12]),
      format!("{label}-{id}"),
    ] {
      if !installed_build_name::valid_label(&build_id) {
        continue;
      }
      let path = self.root.join(&build_id);
      match fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
          return Ok(BuildSlot::Vacant(build_id));
        }
        Err(error) => return Err(error),
      }
      let found = installed_content_id(&path, &build_id)?;
      if found == id {
        return installed(&path, &build_id, found).map(BuildSlot::Installed);
      }
    }
    Err(invalid("no unused ctream build location"))
  }

  fn publish(&self, staging: &Temporary, build_id: &str, id: String) -> io::Result<Installation> {
    let mut file = new_file(&staging.0.join(CONTENT_ID_FILE))?;
    writeln!(file, "{id}")?;
    file.sync_all()?;
    drop(file);
    installed(&staging.0, build_id, id.clone())?;
    for name in directories().rev() {
      private_fs::sync_directory(&staging.0.join(name))?;
    }
    let destination = self.root.join(build_id);
    fs::rename(&staging.0, &destination)?;
    private_fs::sync_directory(self.root)?;
    installed(&destination, build_id, id)
  }

  fn select(&self, installation: Installation) -> io::Result<Installation> {
    if self.current.as_deref() != Some(installation.build_id.as_str()) {
      activate(self.root, &installation.build_id)?;
    }
    Ok(installation)
  }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevelopmentReceipt {
  target: String,
  files: BTreeMap<String, String>,
}

impl DevelopmentReceipt {
  fn validate_names<T>(files: &BTreeMap<String, T>) -> io::Result<()> {
    let binaries = binary_names();
    if !binaries.iter().all(|name| files.contains_key(*name)) {
      return Err(invalid(
        "ctream development requires a native preview and CLI",
      ));
    }
    let agents: BTreeSet<_> = agent_paths().collect();
    for name in files.keys() {
      if !binaries.contains(&name.as_str()) && !agents.contains(name) {
        return Err(invalid(format!("invalid ctream development file: {name}")));
      }
    }
    for target in AgentTarget::ALL {
      if !files.contains_key(&target.relative_path(target.agent())) {
        for helper in target.companions() {
          let name = target.relative_path(helper);
          if files.contains_key(&name) {
            return Err(invalid(format!(
              "Wayland helper needs its matching ctream agent: {name}"
            )));
          }
        }
      }
    }
    Ok(())
  }

  fn id(&self) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctream-development-v1\0");
    hash.update(self.target.as_bytes());
    hash.update([0]);
    for (name, digest) in &self.files {
      hash.update(name.as_bytes());
      hash.update([0]);
      hash.update(digest.as_bytes());
      hash.update([0]);
    }
    hex::encode(hash.finalize())
  }

  fn read(path: &Path, id: &str) -> io::Result<Option<Self>> {
    let file = match private_fs::options()
      .read(true)
      .open(path.join(DEVELOPMENT_RECEIPT))
    {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
      Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !private_fs::is_private_file(&metadata) || metadata.len() > MAX_RECEIPT_BYTES {
      return Err(invalid("invalid ctream development receipt"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
      return Err(invalid("ctream development receipt exceeds its size limit"));
    }
    let receipt: Self = serde_json::from_slice(&bytes).map_err(invalid)?;
    Self::validate_names(&receipt.files)?;
    if receipt.target != native_target()?
      || !receipt.files.values().all(|digest| valid_id(digest))
      || receipt.id() != id
    {
      return Err(invalid(
        "ctream development receipt does not match its installation",
      ));
    }
    Ok(Some(receipt))
  }
}

fn binary_names() -> [&'static str; 2] {
  viewer_executables(paths::native_target())
}

fn payloads() -> impl Iterator<Item = String> {
  binary_names()
    .into_iter()
    .map(str::to_owned)
    .chain(agent_paths())
}

fn directories() -> impl DoubleEndedIterator<Item = String> {
  [String::new(), AGENT_DIRECTORY.into()].into_iter().chain(
    AgentTarget::ALL
      .into_iter()
      .map(AgentTarget::relative_directory),
  )
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn directory(path: &Path) -> io::Result<()> {
  if !private_fs::is_directory(&fs::symlink_metadata(path)?) {
    return Err(invalid(format!(
      "ctream directory must be owned and protected from writes by others: {}",
      path.display()
    )));
  }
  Ok(())
}

fn ensure_directory(path: &Path) -> io::Result<()> {
  match private_fs::create_directory(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => directory(path),
    Err(error) => Err(error),
  }
}

fn storage(root: &Path, create: bool) -> io::Result<bool> {
  let anchor = paths::managed_storage_anchor(root)?;
  let relative = root
    .strip_prefix(anchor)
    .map_err(|_| invalid("ctream storage is outside its managed account"))?;
  let mut path = anchor.to_owned();
  // The account directory itself must already exist. Apply the same ownership
  // checks as the ctl runtime store at every component below it.
  for component in std::iter::once(None).chain(relative.components().map(Some)) {
    let result = if let Some(component) = component {
      path.push(component.as_os_str());
      if create {
        ensure_directory(&path)
      } else {
        directory(&path)
      }
    } else {
      directory(&path)
    };
    match result {
      Ok(()) => {}
      Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(false),
      Err(error) => return Err(error),
    }
  }
  Ok(true)
}

fn lock(root: &Path) -> io::Result<File> {
  let file = private_fs::options()
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .open(root.join("install.lock"))?;
  if !private_fs::is_private_file(&file.metadata()?) {
    return Err(invalid("ctream install lock must be a private owned file"));
  }
  file.lock()?;
  Ok(file)
}

fn read_current(root: &Path) -> io::Result<Option<String>> {
  let file = match private_fs::options().read(true).open(root.join("current")) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  let name = read_marker(file, installed_build_name::MAX_LABEL_BYTES)?;
  if !valid_build_id(&name) {
    return Err(invalid("invalid ctream installation selection"));
  }
  Ok(Some(name))
}

fn valid_build_id(name: &str) -> bool {
  valid_id(name) || installed_build_name::valid_label(name)
}

fn installed_content_id(path: &Path, build_id: &str) -> io::Result<String> {
  directory(path)?;
  if valid_id(build_id) {
    return Ok(build_id.to_owned());
  }
  let file = private_fs::options()
    .read(true)
    .open(path.join(CONTENT_ID_FILE))?;
  let id = read_marker(file, CONTENT_ID_BYTES)?;
  if !valid_id(&id) {
    return Err(invalid("invalid ctream content ID"));
  }
  Ok(id)
}

fn read_marker(file: File, max: usize) -> io::Result<String> {
  if !private_fs::is_private_file(&file.metadata()?) {
    return Err(invalid(
      "ctream installation marker must be a private owned file",
    ));
  }
  let mut id = String::new();
  file.take(max as u64 + 2).read_to_string(&mut id)?;
  if id.ends_with('\n') {
    id.pop();
  }
  Ok(id)
}

fn valid_id(id: &str) -> bool {
  id.len() == CONTENT_ID_BYTES
    && id
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn new_file(path: &Path) -> io::Result<File> {
  private_fs::options()
    .create_new(true)
    .write(true)
    .open(path)
}

fn activate(root: &Path, build_id: &str) -> io::Result<()> {
  let path = root.join(format!(".current-{}", uuid::Uuid::new_v4()));
  let mut file = new_file(&path)?;
  let staged = Temporary(path);
  writeln!(file, "{build_id}")?;
  file.sync_all()?;
  drop(file);
  fs::rename(&staged.0, root.join("current"))?;
  private_fs::sync_directory(root)
}

fn archive_bytes(path: &Path) -> io::Result<Vec<u8>> {
  if !fs::metadata(path)?.is_file() {
    return Err(invalid("ctream archive must be a regular ZIP file"));
  }
  let file = File::open(path)?;
  let file_type = file.metadata()?;
  if !file_type.is_file() || file_type.len() > MAX_ARCHIVE_BYTES {
    return Err(invalid(
      "ctream archive must be a regular ZIP within its size limit",
    ));
  }
  let mut bytes = Vec::new();
  file.take(MAX_ARCHIVE_BYTES + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
    return Err(invalid("ctream archive exceeds its size limit"));
  }
  Ok(bytes)
}

fn installed(path: &Path, build_id: &str, id: String) -> io::Result<Installation> {
  directory(path)?;
  for name in directories().skip(1) {
    directory(&path.join(name))?;
  }
  let development = DevelopmentReceipt::read(path, &id)?;
  for name in payloads() {
    let metadata = fs::symlink_metadata(path.join(&name));
    if development
      .as_ref()
      .is_some_and(|receipt| !receipt.files.contains_key(&name))
    {
      match metadata {
        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
        Ok(_) => {
          return Err(invalid(format!(
            "unexpected executable in ctream development installation: {name}"
          )));
        }
        Err(error) => return Err(error),
      }
    }
    let metadata = metadata?;
    if !private_fs::is_private_file(&metadata) || !private_fs::is_executable(&metadata) {
      return Err(invalid(format!(
        "invalid executable in ctream installation: {name}"
      )));
    }
  }
  let [preview, cli] = binary_names().map(|name| path.join(name));
  Ok(Installation {
    id,
    build_id: build_id.to_owned(),
    development: development.is_some(),
    directory: path.to_owned(),
    preview,
    cli,
  })
}

fn snapshot_file(source: &Path, destination: &Path, remaining: u64) -> io::Result<(u64, String)> {
  let invalid_source = || {
    invalid(format!(
      "invalid ctream development executable: {}",
      source.display()
    ))
  };
  if !fs::metadata(source)?.is_file() {
    return Err(invalid_source());
  }
  let input = File::open(source)?;
  let metadata = input.metadata()?;
  let size = metadata.len();
  if !metadata.is_file() || !(1..=MAX_AGENT_BYTES.min(remaining)).contains(&size) {
    return Err(invalid_source());
  }
  let mut input = input.take(size + 1);
  let mut output = new_file(destination)?;
  let mut hash = Sha256::new();
  let mut buffer = [0; 8192];
  let mut copied = 0_u64;
  loop {
    let count = input.read(&mut buffer)?;
    if count == 0 {
      break;
    }
    copied += count as u64;
    hash.update(&buffer[..count]);
    output.write_all(&buffer[..count])?;
  }
  if copied != size {
    return Err(invalid(format!(
      "ctream development executable changed during installation: {}",
      source.display()
    )));
  }
  private_fs::set_executable(&output)?;
  output.sync_all()?;
  Ok((size, hex::encode(hash.finalize())))
}

fn extract(
  archive: &mut zip::ZipArchive<impl io::Read + io::Seek>,
  target: &str,
  staging: &Path,
) -> io::Result<()> {
  let payloads: BTreeSet<_> = payloads().collect();
  let directories: BTreeSet<_> = directories().collect();
  if !(payloads.len()..=payloads.len() + directories.len()).contains(&archive.len()) {
    return Err(invalid(
      "ctream ZIP does not contain the expected preview and agents",
    ));
  }
  let prefix = format!("{}/", archive_name(target));
  let mut seen = BTreeSet::new();
  let mut total = 0_u64;
  for index in 0..archive.len() {
    let mut file = archive.by_index(index).map_err(invalid)?;
    let raw = std::str::from_utf8(file.name_raw()).map_err(invalid)?;
    let name = raw
      .strip_prefix(&prefix)
      .ok_or_else(|| invalid(format!("ctream ZIP entries must be inside {prefix}")))?;
    let mode = file.unix_mode().unwrap_or(0) & 0o170_000;
    if !seen.insert(name.to_owned()) {
      return Err(invalid("duplicate entry in ctream ZIP"));
    }
    if (name.is_empty()
      || name
        .strip_suffix('/')
        .is_some_and(|dir| directories.contains(dir)))
      && file.is_dir()
      && matches!(mode, 0 | 0o040_000)
    {
      if file.size() != 0 {
        return Err(invalid("ctream ZIP directory contains data"));
      }
      continue;
    }
    if !payloads.contains(name) || !file.is_file() || !matches!(mode, 0 | 0o100_000) {
      return Err(invalid(format!("unexpected ctream ZIP entry: {name}")));
    }
    let size = file.size();
    if !(1..=MAX_AGENT_BYTES).contains(&size) || size > MAX_PAYLOAD_BYTES - total {
      return Err(invalid(format!(
        "ctream ZIP entry has an invalid size: {name}"
      )));
    }
    total += size;
    let path = staging.join(name);
    let mut output = new_file(&path)?;
    let copied = io::copy(&mut file.by_ref().take(size + 1), &mut output)?;
    if copied != size {
      return Err(invalid(format!(
        "ctream ZIP entry size mismatch: {}",
        path.display()
      )));
    }
    private_fs::set_executable(&output)?;
    output.sync_all()?;
  }
  if !payloads.is_subset(&seen) {
    return Err(invalid("ctream ZIP is missing a required executable"));
  }
  Ok(())
}

struct Temporary(PathBuf);

impl Drop for Temporary {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
    let _ = fs::remove_dir_all(&self.0);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use zip::write::SimpleFileOptions;

  fn workspace() -> Temporary {
    let path = std::env::temp_dir().join(format!("ctlup-ctream-{}", uuid::Uuid::new_v4()));
    private_fs::create_directory(&path).unwrap();
    Temporary(path)
  }

  fn fixture(
    path: &Path,
    target: &str,
    payload: &[u8],
    replace: Option<&str>,
    extra: Option<&str>,
  ) {
    let folder = archive_name(target);
    let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
    // Skip explicit directory entries in the unsafe-path test so the entry
    // count stays within its limit and exercises path validation.
    if extra.is_none() {
      for directory in directories() {
        let name = if directory.is_empty() {
          format!("{folder}/")
        } else {
          format!("{folder}/{directory}/")
        };
        writer
          .add_directory(name, SimpleFileOptions::default())
          .unwrap();
      }
    }
    for name in payloads().chain(extra.map(str::to_owned)) {
      if Some(name.as_str()) == replace {
        writer
          .add_symlink(
            format!("{folder}/{name}"),
            "../../../escaped",
            SimpleFileOptions::default(),
          )
          .unwrap();
      } else {
        writer
          .start_file(format!("{folder}/{name}"), SimpleFileOptions::default())
          .unwrap();
        writer.write_all(payload).unwrap();
      }
    }
    writer.finish().unwrap();
  }

  fn development_inputs(home: &Path) -> BTreeMap<String, PathBuf> {
    binary_names()
      .into_iter()
      .map(|name| {
        let source = home.join(format!("build-{name}"));
        fs::write(&source, name).unwrap();
        (name.into(), source)
      })
      .collect()
  }

  fn build_metadata(channel: &str) -> BuildMetadata {
    BuildMetadata {
      version: Some("0.1.0".into()),
      revision: Some("a".repeat(40)),
      built_at: Some("2026-11-22T08:15:25Z".into()),
      channel: channel.into(),
    }
  }

  fn assert_only_generation(root: &Path, installation: &Installation) {
    let directories: Vec<_> = fs::read_dir(root)
      .unwrap()
      .map(Result::unwrap)
      .filter(|entry| entry.file_type().unwrap().is_dir())
      .map(|entry| entry.path())
      .collect();
    assert_eq!(directories, [root.join(&installation.build_id)]);
  }

  #[test]
  fn new_store_starts_without_a_legacy_selection() {
    let home = workspace();
    let profile = home.0.join(".tokn/ctl");
    let root = profile.join("ctlup/store/ctream");
    let legacy = profile.join("ctream");
    let inputs = development_inputs(&home.0);
    let build = build_metadata("local");
    let old = install_development(&legacy.join("installs"), &inputs, &build).unwrap();
    fs::remove_file(old.directory.join(CONTENT_ID_FILE)).unwrap();
    fs::rename(old.directory, legacy.join("installs").join(&old.id)).unwrap();
    activate(&legacy, &old.id).unwrap();
    let config = legacy.join("config.toml");
    fs::write(&config, b"# config fixture\n").unwrap();

    assert!(active(&root).unwrap().is_none());
    assert!(!root.exists());

    let fresh = install_development(&root, &inputs, &build).unwrap();
    assert_eq!(fresh.id, old.id);
    assert_eq!(fresh.directory, root.join(&fresh.build_id));
    assert_eq!(active(&root).unwrap().unwrap().directory, fresh.directory);
    assert_eq!(fs::read(config).unwrap(), b"# config fixture\n");
  }

  #[test]
  fn hash_named_build_remains_launchable_until_the_next_install() {
    let home = workspace();
    let root = home.0.join(".tokn/ctl/ctlup/store/ctream");
    let inputs = development_inputs(&home.0);
    let build = build_metadata("local");
    let named = install_development(&root, &inputs, &build).unwrap();
    fs::remove_file(named.directory.join(CONTENT_ID_FILE)).unwrap();
    let legacy = root.join(&named.id);
    fs::rename(&named.directory, &legacy).unwrap();
    activate(&root, &named.id).unwrap();

    let active_legacy = active(&root).unwrap().unwrap();
    assert_eq!(active_legacy.build_id, named.id);
    assert_eq!(active_legacy.directory, legacy);
    let next = install_development(&root, &inputs, &build).unwrap();
    assert_eq!(next.build_id, "dev-20261122T081525Z-v0.1.0-aaaaaaa-local");
    assert_eq!(next.id, active_legacy.id);
    assert_eq!(active(&root).unwrap().unwrap().directory, next.directory);
    assert!(active_legacy.preview.is_file());
  }

  #[test]
  fn development_can_reuse_a_partial_generation_across_release_switches() {
    let home = workspace();
    let root = home.0.join(".tokn/ctl/ctlup/store/ctream");
    let build = build_metadata("local");
    let mut inputs = development_inputs(&home.0);
    let agent = home.0.join("built-agent");
    let bundled_agent = AgentTarget::LinuxX64.relative_path("ctream-agent");
    let other_agent = AgentTarget::LinuxArm64.relative_path("ctream-agent");
    fs::write(&agent, b"first agent").unwrap();
    inputs.insert(bundled_agent.clone(), agent.clone());

    let developed = install_development(&root, &inputs, &build).unwrap();
    assert!(developed.development);
    assert_eq!(active(&root).unwrap().unwrap().id, developed.id);
    assert_eq!(
      fs::read(developed.directory.join(&bundled_agent)).unwrap(),
      b"first agent"
    );
    assert!(!developed.directory.join(&other_agent).exists());
    let receipt = fs::read_to_string(developed.directory.join(DEVELOPMENT_RECEIPT)).unwrap();
    assert!(!receipt.contains(agent.to_str().unwrap()));

    let archive = home.0.join("ctream.zip");
    fixture(&archive, native_target().unwrap(), b"release", None, None);
    let released = install(&root, &archive, None).unwrap();
    assert!(!released.development);
    assert_ne!(released.id, developed.id);
    assert_eq!(active(&root).unwrap().unwrap().id, released.id);
    assert_eq!(
      fs::read_to_string(&developed.preview).unwrap(),
      binary_names()[0]
    );

    let relocated = home.0.join("relocated-agent");
    fs::rename(agent, &relocated).unwrap();
    inputs.insert(bundled_agent.clone(), relocated.clone());
    assert_eq!(
      install_development(&root, &inputs, &build)
        .unwrap()
        .directory,
      developed.directory
    );
    assert_eq!(active(&root).unwrap().unwrap().id, developed.id);
    assert_eq!(fs::read(&released.preview).unwrap(), b"release");

    fs::write(relocated, b"second agent").unwrap();
    let next = install_development(&root, &inputs, &build).unwrap();
    assert_ne!(next.id, developed.id);
    assert_eq!(
      next.build_id,
      format!("{}-{}", developed.build_id, &next.id[..12])
    );
    assert_eq!(
      install_development(&root, &inputs, &build)
        .unwrap()
        .directory,
      next.directory
    );
    assert_eq!(active(&root).unwrap().unwrap().id, next.id);
    assert_eq!(
      fs::read(developed.directory.join(&bundled_agent)).unwrap(),
      b"first agent"
    );
  }

  #[test]
  fn development_requires_coherent_agent_lanes_and_preserves_active_on_copy_error() {
    let home = workspace();
    let root = home.0.join(".tokn/ctl/ctlup/store/ctream");
    let build = build_metadata("local");
    let mut inputs = development_inputs(&home.0);
    let first = install_development(&root, &inputs, &build).unwrap();
    let helper = home.0.join("input-helper");
    let bundled_helper = AgentTarget::LinuxX64.relative_path("ctream-wayland-input");
    let bundled_agent = AgentTarget::LinuxX64.relative_path("ctream-agent");
    fs::write(&helper, b"helper").unwrap();
    inputs.insert(bundled_helper.clone(), helper);
    assert_eq!(
      install_development(&root, &inputs, &build)
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidData
    );
    inputs.insert(bundled_agent.clone(), home.0.join("missing-agent"));
    assert_eq!(
      install_development(&root, &inputs, &build)
        .unwrap_err()
        .kind(),
      io::ErrorKind::NotFound
    );
    assert_eq!(active(&root).unwrap().unwrap().id, first.id);
    assert_only_generation(&root, &first);

    fs::write(&inputs[&bundled_agent], b"agent").unwrap();
    let next = install_development(&root, &inputs, &build).unwrap();
    assert_eq!(
      fs::read(next.directory.join(&bundled_helper)).unwrap(),
      b"helper"
    );
  }

  #[test]
  fn installs_updates_and_reuses_an_existing_generation() {
    let home = workspace();
    let root = home.0.join(".tokn/ctl/ctlup/store/ctream");
    let archive = home.0.join("ctream.zip");
    let platform = native_target().unwrap();
    assert!(active(&root).unwrap().is_none());

    fixture(&archive, platform, b"first", None, None);
    let first = install(&root, &archive, None).unwrap();
    assert_eq!(
      first.build_id,
      format!("provided-undated-vunknown-{}", &first.id[..12])
    );
    assert_eq!(
      first.directory,
      install(&root, &archive, None).unwrap().directory
    );
    assert_eq!(active(&root).unwrap().unwrap().id, first.id);

    fixture(&archive, platform, b"second", None, None);
    let build = build_metadata("nightly");
    let second = install(&root, &archive, Some(&build)).unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(second.build_id, "nightly-20261122T081525Z-v0.1.0-aaaaaaa");
    assert_eq!(active(&root).unwrap().unwrap().id, second.id);
    assert_eq!(fs::read(&first.preview).unwrap(), b"first");
    assert_eq!(fs::read(&second.cli).unwrap(), b"second");
    for name in agent_paths() {
      let agent = second.directory.join(name);
      assert_eq!(fs::read(&agent).unwrap(), b"second");
      assert!(private_fs::is_executable(&fs::metadata(agent).unwrap()));
    }
  }

  #[test]
  fn unsafe_or_foreign_update_preserves_the_active_payload() {
    let home = workspace();
    let root = home.0.join(".tokn/ctl/ctlup/store/ctream");
    let archive = home.0.join("ctream.zip");
    let platform = native_target().unwrap();
    fixture(&archive, platform, b"first", None, None);
    let first = install(&root, &archive, None).unwrap();

    let bundled_agent = AgentTarget::LinuxX64.relative_path("ctream-agent");
    for (target, replace, extra) in [
      (platform, None, Some("../../escaped")),
      (platform, Some(bundled_agent.as_str()), None),
      ("other-architecture", None, None),
    ] {
      fixture(&archive, target, b"bad", replace, extra);
      assert_eq!(
        install(&root, &archive, None).unwrap_err().kind(),
        io::ErrorKind::InvalidData
      );
      assert_eq!(active(&root).unwrap().unwrap().id, first.id);
      assert_eq!(fs::read(&first.preview).unwrap(), b"first");
      assert_only_generation(&root, &first);
      assert!(!root.join("escaped").exists());
    }
  }

  #[cfg(unix)]
  #[test]
  fn refuses_a_symlink_in_the_store_path() {
    let home = workspace();
    let store = home.0.join(".tokn/ctl/ctlup/store");
    let unrelated = home.0.join("existing");
    let archive = home.0.join("ctream.zip");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    private_fs::create_directory(&unrelated).unwrap();
    std::os::unix::fs::symlink(&unrelated, &store).unwrap();
    fixture(&archive, native_target().unwrap(), b"bundle", None, None);

    assert_eq!(
      install(&store.join("ctream"), &archive, None)
        .unwrap_err()
        .kind(),
      io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read_dir(&unrelated).unwrap().count(), 0);
  }
}
