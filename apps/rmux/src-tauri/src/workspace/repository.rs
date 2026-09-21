use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::{
  HostCatalogSnapshot, UpdateHostsRequest, UpdateWorkspaceRequest, WorkspaceConnectionMethod,
  WorkspaceHost, WorkspaceHostIdentity, WorkspaceSnapshot,
};
use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use sha2::{Digest, Sha256};

pub(super) const MAX_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024;

pub(super) struct Repository {
  pub(super) directory: PathBuf,
  definition_path: PathBuf,
  legacy_directory: Option<PathBuf>,
}

impl Repository {
  pub fn new(directory: PathBuf) -> Self {
    Self {
      definition_path: directory.join("tasks.json"),
      directory,
      legacy_directory: None,
    }
  }

  pub fn with_definition_store(mut self, path: PathBuf) -> Self {
    self.definition_path = path;
    self
  }

  pub fn with_legacy_directory(mut self, directory: PathBuf) -> Self {
    if directory != self.directory {
      self.legacy_directory = Some(directory);
    }
    self
  }

  pub fn load(&self) -> CommandResult<WorkspaceSnapshot> {
    let _lock = self.lock()?;
    self.migrate_location()?;
    self.migrate_document(self.read()?)
  }

  pub fn update(&self, request: UpdateWorkspaceRequest) -> CommandResult<WorkspaceSnapshot> {
    if request.document.schema_version != 8 {
      return Err(CommandErrorDto::new(
        "workspace_version_unsupported",
        "Reload the workspace before saving with this app version.",
      ));
    }
    request.document.validate()?;
    let _lock = self.lock()?;
    self.migrate_location()?;
    // Read and validate even when the caller expects an absent file. Never
    // replace an unreadable, corrupt, unsupported, or concurrently edited file.
    let current = self.migrate_document(self.read()?)?;
    if current.revision != request.expected_revision {
      return Err(CommandErrorDto::new(
        "workspace_conflict",
        "Another app instance changed the workspace. Reload the app before saving further changes.",
      ));
    }
    let snapshot = WorkspaceSnapshot {
      revision: Some(content_revision(&request.document)?),
      document: request.document,
    };
    self.persist_snapshot(&snapshot)?;
    Ok(snapshot)
  }

  pub fn load_hosts(&self) -> CommandResult<HostCatalogSnapshot> {
    let _lock = self.lock()?;
    self.migrate_location()?;
    self.migrate_document(self.read()?)?;
    self.read_catalog()
  }

  pub fn update_hosts(&self, request: UpdateHostsRequest) -> CommandResult<HostCatalogSnapshot> {
    request.document.validate()?;
    let _lock = self.lock()?;
    self.migrate_location()?;
    self.migrate_document(self.read()?)?;
    let current = self.read_catalog()?;
    if current.revision != request.expected_revision {
      return Err(CommandErrorDto::new(
        "hosts_conflict",
        "Another app instance or editor changed the hosts. Reload before saving further changes.",
      ));
    }
    let snapshot = HostCatalogSnapshot {
      revision: Some(content_revision(&request.document)?),
      document: request.document,
    };
    self.persist_catalog(&snapshot)?;
    Ok(snapshot)
  }

  // The destination lock is always acquired first. Older app versions only
  // acquire the legacy lock, so migration cannot deadlock with their writes.
  fn migrate_location(&self) -> CommandResult<()> {
    let Some(directory) = &self.legacy_directory else {
      return Ok(());
    };
    let destination = self.directory.join("workspace.json");
    regular_file_or_absent(&destination).map_err(io_error)?;
    if destination.try_exists().map_err(io_error)? {
      return Ok(());
    }
    let source = directory.join("workspace.json");
    regular_file_or_absent(&source).map_err(io_error)?;
    if !source.try_exists().map_err(io_error)? {
      return Ok(());
    }

    let legacy = Self::new(directory.clone());
    let _legacy_lock = legacy.lock()?;
    if let Some(bytes) = legacy.read_bytes()? {
      // Validate before importing. Copy the exact document and revision so a
      // location change does not invalidate a pending save from this client.
      decode_snapshot(&bytes)?;
      self.write(&bytes).map_err(io_error)?;
    }
    // Retain the source and its backups for recovery. Once a destination
    // exists, it is authoritative even if an older app updates the old path.
    Ok(())
  }

  fn persist_snapshot(&self, snapshot: &WorkspaceSnapshot) -> CommandResult<()> {
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(CommandErrorDto::backend)?;
    if bytes.len() as u64 > MAX_WORKSPACE_BYTES {
      return Err(CommandErrorDto::new(
        "workspace_too_large",
        "The workspace exceeds the size limit.",
      ));
    }
    self.write(&bytes).map_err(io_error)?;
    Ok(())
  }

  fn migrate_document(&self, mut snapshot: WorkspaceSnapshot) -> CommandResult<WorkspaceSnapshot> {
    if snapshot.document.schema_version == 8 {
      return Ok(snapshot);
    }
    if snapshot.document.schema_version == 2 {
      // Preserve the original workspace before writing to either store. Import
      // is idempotent, so retrying after a crash between commits is safe.
      self.ensure_backup("workspace-v2.backup.json")?;
      let imported = task_store::Repository::new(self.definition_path.clone())
        .import_legacy(&snapshot.document.task_definitions)
        .map_err(crate::task_definitions::store_error)?;
      for reference in &mut snapshot.document.task_references {
        let saved = snapshot
          .document
          .task_definitions
          .iter()
          .find(|definition| Some(&definition.definition_id) == reference.definition_id.as_ref());
        if let Some(saved) = saved
          && reference.applied_revision.as_deref() == Some(saved.revision.as_str())
          && let Some(definition) = imported
            .definitions
            .iter()
            .find(|definition| definition.definition_id == saved.definition_id)
        {
          reference.applied_revision = Some(definition.revision.clone());
        }
      }
      snapshot.document.task_definitions.clear();
    } else {
      self.ensure_backup(&format!(
        "workspace-v{}.backup.json",
        snapshot.document.schema_version
      ))?;
    }
    // Import definitions first. Retrying the exact batch after a crash is safe;
    // no host command can edit the imported catalog until migration completes.
    self.import_hosts(&snapshot.document.hosts, &snapshot.document.ssh_gateways)?;
    let referenced: std::collections::HashSet<&str> = snapshot
      .document
      .sessions
      .iter()
      .map(|item| item.host_id.as_str())
      .chain(
        snapshot
          .document
          .task_references
          .iter()
          .map(|item| item.host_id.as_str()),
      )
      .chain(
        snapshot
          .document
          .port_forwards
          .iter()
          .map(|item| item.host_id.as_str()),
      )
      .collect();
    snapshot.document.host_identities = snapshot
      .document
      .hosts
      .iter()
      .filter(|host| referenced.contains(host.host_id.as_str()))
      .filter_map(|host| {
        host
          .remote_info
          .as_ref()
          .map(|remote_info| WorkspaceHostIdentity {
            host_id: host.host_id.clone(),
            remote_info: remote_info.clone(),
          })
      })
      .collect();
    snapshot.document.hosts.clear();
    snapshot.document.ssh_gateways.clear();
    snapshot.document.schema_version = 8;
    snapshot.revision = Some(content_revision(&snapshot.document)?);
    snapshot.document.validate()?;
    self.persist_snapshot(&snapshot)?;
    Ok(snapshot)
  }

  fn ensure_backup(&self, name: &str) -> CommandResult<()> {
    let path = self.directory.join(name);
    regular_file_or_absent(&path).map_err(io_error)?;
    let source = fs::read(self.directory.join("workspace.json")).map_err(io_error)?;
    match fs::read(&path) {
      Ok(existing) if existing == source => return Ok(()),
      Ok(_) => {
        return Err(CommandErrorDto::new(
          "workspace_backup_conflict",
          format!(
            "{name} differs from the workspace being migrated. Preserve and review both files before retrying."
          ),
        ));
      }
      Err(error) if error.kind() == io::ErrorKind::NotFound => {}
      Err(error) => return Err(io_error(error)),
    }
    self.write_named(name, &source).map_err(io_error)
  }

  fn read(&self) -> CommandResult<WorkspaceSnapshot> {
    let Some(bytes) = self.read_bytes()? else {
      return Ok(WorkspaceSnapshot::default());
    };
    let (snapshot, migrated_v1) = decode_snapshot(&bytes)?;
    if migrated_v1 {
      self.ensure_backup("workspace-v1.backup.json")?;
    }
    Ok(snapshot)
  }

  fn read_bytes(&self) -> CommandResult<Option<Vec<u8>>> {
    let path = self.directory.join("workspace.json");
    regular_file_or_absent(&path).map_err(io_error)?;
    let file = match File::open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return Ok(None);
      }
      Err(error) => return Err(io_error(error)),
    };
    let mut bytes = Vec::new();
    file
      .take(MAX_WORKSPACE_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(io_error)?;
    if bytes.len() as u64 > MAX_WORKSPACE_BYTES {
      return Err(CommandErrorDto::new(
        "workspace_too_large",
        "The workspace file is too large. It has not been changed.",
      ));
    }
    Ok(Some(bytes))
  }

  fn lock(&self) -> CommandResult<File> {
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      directory.mode(0o700);
    }
    directory.create(&self.directory).map_err(io_error)?;
    let path = self.directory.join("workspace.lock");
    regular_file_or_absent(&path).map_err(io_error)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600);
    }
    let file = options.open(path).map_err(io_error)?;
    file.lock().map_err(io_error)?;
    Ok(file)
  }

  fn write(&self, bytes: &[u8]) -> io::Result<()> {
    self.write_named("workspace.json", bytes)
  }

  pub(super) fn write_named(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
    regular_file_or_absent(&self.directory.join(name))?;
    let path = self
      .directory
      .join(format!(".{name}-{}.tmp", uuid::Uuid::new_v4()));
    let temporary = TemporaryFile(path);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600);
    }
    let mut file = options.open(&temporary.0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary.0, self.directory.join(name))?;
    #[cfg(unix)]
    File::open(&self.directory)?.sync_all()?;
    Ok(())
  }
}

fn decode_snapshot(bytes: &[u8]) -> CommandResult<(WorkspaceSnapshot, bool)> {
  let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
    CommandErrorDto::new(
      "workspace_unreadable",
      format!("Could not read workspace.json; the file has been preserved: {error}"),
    )
  })?;
  let migrated_v1 = value["document"]["schema_version"] == 1;
  if migrated_v1 {
    value["document"]["schema_version"] = 2.into();
    if let Some(tabs) = value["document"]["tabs"].as_array_mut() {
      for tab in tabs {
        if let Some(tab) = tab.as_object_mut() {
          tab.insert("kind".into(), "session".into());
        }
      }
    }
    if let Some(tab) = value["document"]["active_tab"].as_object_mut() {
      tab.insert("kind".into(), "session".into());
    }
  }
  if value["document"]["schema_version"]
    .as_u64()
    .is_some_and(|version| (2..=6).contains(&version))
  {
    normalize_legacy_hosts(&mut value["document"]["hosts"])?;
  }
  let mut snapshot: WorkspaceSnapshot =
    serde_json::from_value(value).map_err(CommandErrorDto::backend)?;
  snapshot.document.validate()?;
  if snapshot.revision.as_ref().is_none_or(String::is_empty) {
    return Err(CommandErrorDto::new(
      "workspace_invalid",
      "The workspace file has no revision. It has not been changed.",
    ));
  }
  if snapshot.document.schema_version == 8 {
    // The stored revision is informative; content is authoritative when an
    // external editor changes JSON without updating that field.
    snapshot.revision = Some(content_revision(&snapshot.document)?);
  }
  Ok((snapshot, migrated_v1))
}

/// Convert only the old shape; already-normalized fixtures and documents retain
/// all their fields and still pass through the current strict deserializer.
fn normalize_legacy_hosts(hosts: &mut serde_json::Value) -> CommandResult<()> {
  #[derive(serde::Deserialize)]
  #[serde(deny_unknown_fields)]
  struct LegacyHost {
    host_id: String,
    target: ConnectionTargetDto,
  }

  let Some(hosts) = hosts.as_array_mut() else {
    return Ok(());
  };
  for host in hosts {
    if host.get("target").is_none() {
      continue;
    }
    let legacy: LegacyHost =
      serde_json::from_value(host.clone()).map_err(CommandErrorDto::backend)?;
    let mut migrated = WorkspaceHost {
      host_id: legacy.host_id,
      name: "Local".into(),
      connection_methods: Vec::new(),
      preferred_method_id: None,
      remote_info: None,
    };
    let mut target = legacy.target;
    if let ConnectionTargetDto::Ssh {
      destination,
      remote_info,
      ..
    } = &mut target
    {
      migrated.name.clone_from(destination);
      migrated.remote_info = remote_info.take();
      migrated.preferred_method_id = Some("default".into());
      migrated.connection_methods.push(WorkspaceConnectionMethod {
        method_id: "default".into(),
        name: "SSH".into(),
        target,
      });
    }
    *host = serde_json::to_value(migrated).map_err(CommandErrorDto::backend)?;
  }
  Ok(())
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ignored = fs::remove_file(&self.0);
  }
}

pub(super) fn regular_file_or_absent(path: &Path) -> io::Result<()> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.is_file() => Ok(()),
    Ok(_) => Err(io::Error::other(
      "Workspace paths must be regular files, not symlinks or directories.",
    )),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

// Owned adapter for Result::map_err.
#[allow(clippy::needless_pass_by_value)]
fn io_error(error: io::Error) -> CommandErrorDto {
  CommandErrorDto::new(
    "workspace_io_failed",
    format!("Could not access workspace.json: {error}"),
  )
}

/// Content revisions detect external edits that leave the stored token intact.
pub(super) fn content_revision(value: &impl serde::Serialize) -> CommandResult<String> {
  let bytes = serde_json::to_vec(value).map_err(CommandErrorDto::backend)?;
  Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}
