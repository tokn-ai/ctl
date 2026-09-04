use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::{UpdateWorkspaceRequest, WorkspaceSnapshot};
use crate::error::{CommandErrorDto, CommandResult};

const MAX_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024;

pub(super) struct Repository {
  directory: PathBuf,
  definition_path: PathBuf,
}

impl Repository {
  pub fn new(directory: PathBuf) -> Self {
    Self {
      definition_path: directory.join("tasks.json"),
      directory,
    }
  }

  pub fn with_definition_store(mut self, path: PathBuf) -> Self {
    self.definition_path = path;
    self
  }

  pub fn load(&self) -> CommandResult<WorkspaceSnapshot> {
    let _lock = self.lock()?;
    self.migrate_definitions(self.read()?)
  }

  pub fn update(&self, request: UpdateWorkspaceRequest) -> CommandResult<WorkspaceSnapshot> {
    if request.document.schema_version != 3 {
      return Err(CommandErrorDto::new(
        "workspace_version_unsupported",
        "Reload the workspace before saving with this app version.",
      ));
    }
    request.document.validate()?;
    let _lock = self.lock()?;
    // Read and validate even when the caller expects an absent file. Never
    // replace an unreadable, corrupt, unsupported, or concurrently edited file.
    let current = self.migrate_definitions(self.read()?)?;
    if current.revision != request.expected_revision {
      return Err(CommandErrorDto::new(
        "workspace_conflict",
        "Another app instance changed the workspace. Reload the app before saving further changes.",
      ));
    }
    let snapshot = WorkspaceSnapshot {
      revision: Some(uuid::Uuid::new_v4().to_string()),
      document: request.document,
    };
    self.persist_snapshot(&snapshot)?;
    Ok(snapshot)
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

  fn migrate_definitions(
    &self,
    mut snapshot: WorkspaceSnapshot,
  ) -> CommandResult<WorkspaceSnapshot> {
    if snapshot.document.schema_version == 3 {
      return Ok(snapshot);
    }
    // Preserve the original workspace before writing to either store. Import is
    // idempotent, so retrying after a crash between the two commits is safe.
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
    snapshot.document.schema_version = 3;
    snapshot.revision = Some(uuid::Uuid::new_v4().to_string());
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
    let path = self.directory.join("workspace.json");
    regular_file_or_absent(&path).map_err(io_error)?;
    let file = match File::open(&path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => {
        return Ok(WorkspaceSnapshot::default());
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
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
      CommandErrorDto::new(
        "workspace_unreadable",
        format!("Could not read workspace.json; the file has been preserved: {error}"),
      )
    })?;
    if value["document"]["schema_version"] == 1 {
      self.ensure_backup("workspace-v1.backup.json")?;
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
    let snapshot: WorkspaceSnapshot =
      serde_json::from_value(value).map_err(CommandErrorDto::backend)?;
    snapshot.document.validate()?;
    if snapshot.revision.as_ref().is_none_or(String::is_empty) {
      return Err(CommandErrorDto::new(
        "workspace_invalid",
        "The workspace file has no revision. It has not been changed.",
      ));
    }
    Ok(snapshot)
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

  fn write_named(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
    let path = self
      .directory
      .join(format!(".workspace-{}.tmp", uuid::Uuid::new_v4()));
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

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ignored = fs::remove_file(&self.0);
  }
}

fn regular_file_or_absent(path: &Path) -> io::Result<()> {
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
