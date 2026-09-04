use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use task_proto::TaskDefinition;

use crate::{SavedTaskDefinition, Snapshot, StoreError, io_error};

const SCHEMA_VERSION: u64 = 1;
const MAX_STORE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
  schema_version: u64,
  definitions: Vec<SavedTaskDefinition>,
}

/// A scoped definition file shared safely by multiple CLI and desktop processes.
#[derive(Debug, Clone)]
pub struct Repository {
  path: PathBuf,
}

impl Repository {
  #[must_use]
  pub fn new(path: PathBuf) -> Self {
    Self { path }
  }

  /// Loads a consistent snapshot without creating missing files or directories.
  ///
  /// # Errors
  /// Returns validation or I/O errors without modifying an unreadable file.
  pub fn load(&self) -> Result<Snapshot, StoreError> {
    self.read()
  }

  /// Creates a definition or replaces the exact revision the caller observed.
  ///
  /// `None` means create only. Updating preserves the definition ID. Saving
  /// identical values is a no-op and preserves its content-derived revision.
  ///
  /// # Errors
  /// Returns errors for stale revisions, duplicate scoped names, invalid data,
  /// unreadable files, or failed durable writes. Existing definitions are preserved.
  pub fn save(
    &self,
    definition_id: &str,
    expected_revision: Option<&str>,
    definition: TaskDefinition,
  ) -> Result<SavedTaskDefinition, StoreError> {
    validate_definition(definition_id, &definition)?;
    let _lock = self.lock()?;
    let mut snapshot = self.read()?;
    let existing = snapshot
      .definitions
      .iter()
      .position(|saved| saved.definition_id == definition_id);
    match (existing, expected_revision) {
      (None, None) => {}
      (Some(index), Some(expected)) if snapshot.definitions[index].revision == expected => {}
      _ => {
        return Err(StoreError::Conflict {
          definition_id: definition_id.into(),
        });
      }
    }
    if snapshot
      .definitions
      .iter()
      .any(|saved| saved.definition_id != definition_id && saved.definition.name == definition.name)
    {
      return Err(StoreError::NameConflict {
        name: definition.name,
      });
    }
    let saved = SavedTaskDefinition {
      definition_id: definition_id.into(),
      revision: self.revision(&definition)?,
      definition,
    };
    if let Some(index) = existing {
      if snapshot.definitions[index] == saved {
        return Ok(saved);
      }
      snapshot.definitions[index] = saved.clone();
    } else {
      snapshot.definitions.push(saved.clone());
    }
    self.write(snapshot)?;
    Ok(saved)
  }

  /// Removes one definition only if it still matches the observed revision.
  ///
  /// # Errors
  /// Returns errors for missing definitions, stale revisions, or failed storage operations.
  pub fn remove(&self, definition_id: &str, expected_revision: &str) -> Result<(), StoreError> {
    let _lock = self.lock()?;
    let mut snapshot = self.read()?;
    let index = snapshot
      .definitions
      .iter()
      .position(|saved| saved.definition_id == definition_id)
      .ok_or_else(|| StoreError::NotFound {
        definition_id: definition_id.into(),
      })?;
    if snapshot.definitions[index].revision != expected_revision {
      return Err(StoreError::Conflict {
        definition_id: definition_id.into(),
      });
    }
    snapshot.definitions.remove(index);
    self.write(snapshot)
  }

  /// Imports a batch without overwriting any existing definition or partial writes.
  ///
  /// Reimporting the same ID and values succeeds regardless of the legacy revision.
  /// IDs are preserved; returned revisions are derived from the imported values.
  ///
  /// # Errors
  /// Returns errors for conflicting IDs or names, invalid definitions, or failed
  /// storage operations. A failed batch leaves every existing definition unchanged.
  pub fn import_legacy(&self, definitions: &[SavedTaskDefinition]) -> Result<Snapshot, StoreError> {
    for saved in definitions {
      validate_definition(&saved.definition_id, &saved.definition)?;
    }
    let _lock = self.lock()?;
    let mut snapshot = self.read()?;
    let mut changed = false;
    for saved in definitions {
      if let Some(existing) = snapshot
        .definitions
        .iter()
        .find(|existing| existing.definition_id == saved.definition_id)
      {
        if existing.definition != saved.definition {
          return Err(StoreError::Conflict {
            definition_id: saved.definition_id.clone(),
          });
        }
        continue;
      }
      if snapshot
        .definitions
        .iter()
        .any(|existing| existing.definition.name == saved.definition.name)
      {
        return Err(StoreError::NameConflict {
          name: saved.definition.name.clone(),
        });
      }
      snapshot.definitions.push(SavedTaskDefinition {
        definition_id: saved.definition_id.clone(),
        revision: self.revision(&saved.definition)?,
        definition: saved.definition.clone(),
      });
      changed = true;
    }
    sort_definitions(&mut snapshot);
    if changed {
      self.write(snapshot.clone())?;
    }
    Ok(snapshot)
  }

  fn read(&self) -> Result<Snapshot, StoreError> {
    regular_file_or_absent(&self.path)?;
    let file = match File::open(&self.path) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Snapshot::default()),
      Err(error) => return Err(io_error(&self.path, error)),
    };
    let mut bytes = Vec::new();
    file
      .take(MAX_STORE_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|error| io_error(&self.path, error))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
      return Err(StoreError::TooLarge);
    }
    let value: serde_json::Value =
      serde_json::from_slice(&bytes).map_err(|source| self.json_error(source))?;
    if let Some(version) = value
      .get("schema_version")
      .and_then(serde_json::Value::as_u64)
      && version != SCHEMA_VERSION
    {
      return Err(StoreError::UnsupportedVersion { version });
    }
    reject_unknown_definition_fields(&value)?;
    let document: Document =
      serde_json::from_value(value).map_err(|source| self.json_error(source))?;
    let mut snapshot = Snapshot {
      definitions: document.definitions,
    };
    let mut identities = HashSet::new();
    let mut names = HashSet::new();
    for saved in &mut snapshot.definitions {
      validate_definition(&saved.definition_id, &saved.definition)?;
      if !identities.insert(&saved.definition_id) {
        return Err(StoreError::InvalidDefinition {
          message: format!("duplicate definition ID {:?}", saved.definition_id),
        });
      }
      if !names.insert(&saved.definition.name) {
        return Err(StoreError::NameConflict {
          name: saved.definition.name.clone(),
        });
      }
      saved.revision = self.revision(&saved.definition)?;
    }
    sort_definitions(&mut snapshot);
    Ok(snapshot)
  }

  fn revision(&self, definition: &TaskDefinition) -> Result<String, StoreError> {
    let bytes = serde_json::to_vec(definition).map_err(|source| self.json_error(source))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
  }

  fn lock(&self) -> Result<File, StoreError> {
    let parent = parent_directory(&self.path);
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::DirBuilderExt as _;
      directory.mode(0o700);
    }
    directory
      .create(parent)
      .map_err(|error| io_error(parent, error))?;
    let file_name = self
      .path
      .file_name()
      .ok_or_else(|| StoreError::UnsafePath {
        path: self.path.clone(),
      })?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".lock");
    let lock_path = parent.join(lock_name);
    regular_file_or_absent(&lock_path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600);
    }
    let file = options
      .open(&lock_path)
      .map_err(|error| io_error(&lock_path, error))?;
    file.lock().map_err(|error| io_error(lock_path, error))?;
    Ok(file)
  }

  fn write(&self, mut snapshot: Snapshot) -> Result<(), StoreError> {
    sort_definitions(&mut snapshot);
    let bytes = serde_json::to_vec_pretty(&Document {
      schema_version: SCHEMA_VERSION,
      definitions: snapshot.definitions,
    })
    .map_err(|source| self.json_error(source))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
      return Err(StoreError::TooLarge);
    }
    // Recheck under the writer lock so an invalid target is never replaced.
    regular_file_or_absent(&self.path)?;
    let parent = parent_directory(&self.path);
    let temporary = TemporaryFile(parent.join(format!(".tasks-{}.tmp", uuid::Uuid::new_v4())));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt as _;
      options.mode(0o600);
    }
    let mut file = options
      .open(&temporary.0)
      .map_err(|error| io_error(&temporary.0, error))?;
    file
      .write_all(&bytes)
      .and_then(|()| file.sync_all())
      .map_err(|error| io_error(&temporary.0, error))?;
    drop(file);
    fs::rename(&temporary.0, &self.path).map_err(|error| io_error(&self.path, error))?;
    #[cfg(unix)]
    File::open(parent)
      .and_then(|directory| directory.sync_all())
      .map_err(|error| io_error(parent, error))?;
    Ok(())
  }

  fn json_error(&self, source: serde_json::Error) -> StoreError {
    StoreError::Json {
      path: self.path.clone(),
      source,
    }
  }
}

fn reject_unknown_definition_fields(document: &serde_json::Value) -> Result<(), StoreError> {
  // The network TaskDefinition accepts unknown fields for protocol compatibility.
  // File storage must reject them rather than discard settings when rewriting.
  const FIELDS: &[&str] = &[
    "name",
    "program",
    "arguments",
    "working_directory",
    "execution_mode",
  ];
  if let Some(definitions) = document
    .get("definitions")
    .and_then(serde_json::Value::as_array)
  {
    for saved in definitions {
      if let Some(fields) = saved
        .get("definition")
        .and_then(serde_json::Value::as_object)
        && let Some(field) = fields
          .keys()
          .find(|field| !FIELDS.contains(&field.as_str()))
      {
        return Err(StoreError::InvalidDefinition {
          message: format!("unsupported field {field:?}; the definition file was preserved"),
        });
      }
    }
  }
  Ok(())
}

fn validate_definition(definition_id: &str, definition: &TaskDefinition) -> Result<(), StoreError> {
  let invalid = |message: &str| StoreError::InvalidDefinition {
    message: message.into(),
  };
  if definition_id.is_empty()
    || definition_id.len() > 4096
    || definition_id.chars().any(char::is_control)
  {
    return Err(invalid(
      "definition ID must be nonempty text without control characters",
    ));
  }
  if definition.name.is_empty()
    || definition.name.len() > 64
    || definition.name.trim() != definition.name
    || definition.name.chars().any(char::is_control)
  {
    return Err(invalid(
      "name must contain 1–64 bytes without control characters or surrounding whitespace",
    ));
  }
  if definition.program.is_empty() || definition.program.contains('\0') {
    return Err(invalid(
      "program must be nonempty and cannot contain null characters",
    ));
  }
  if definition.arguments.len() > 4096
    || definition
      .arguments
      .iter()
      .any(|argument| argument.len() > 65536 || argument.contains('\0'))
    || definition
      .working_directory
      .as_ref()
      .is_some_and(|directory| directory.contains('\0'))
  {
    return Err(invalid(
      "arguments or working directory exceed limits or contain null characters",
    ));
  }
  Ok(())
}

fn sort_definitions(snapshot: &mut Snapshot) {
  snapshot.definitions.sort_by(|left, right| {
    left
      .definition
      .name
      .cmp(&right.definition.name)
      .then_with(|| left.definition_id.cmp(&right.definition_id))
  });
}

fn parent_directory(path: &Path) -> &Path {
  path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."))
}

fn regular_file_or_absent(path: &Path) -> Result<(), StoreError> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.is_file() => Ok(()),
    Ok(_) => Err(StoreError::UnsafePath { path: path.into() }),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(io_error(path, error)),
  }
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ignored = fs::remove_file(&self.0);
  }
}
