//! Editable, app-local keybindings. No terminal or remote state belongs here.
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::Manager as _;

use crate::error::{CommandErrorDto, CommandResult};

const MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keybinding {
  pub code: String,
  pub primary: bool,
  #[serde(default)]
  pub shift: bool,
  #[serde(default)]
  pub alt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeybindingOverride {
  pub command_id: String,
  #[serde(deserialize_with = "Option::deserialize")]
  pub keybinding: Option<Keybinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeybindingsDocument {
  pub schema_version: u32,
  pub overrides: Vec<KeybindingOverride>,
}

impl Default for KeybindingsDocument {
  fn default() -> Self {
    Self {
      schema_version: 1,
      overrides: Vec::new(),
    }
  }
}

impl KeybindingsDocument {
  fn validate(&self) -> CommandResult<()> {
    if self.schema_version != 1 {
      return Err(error(
        "Unsupported keybindings version; the file has not been changed.",
      ));
    }
    let mut ids = HashSet::new();
    if self.overrides.len() > 128
      || self.overrides.iter().any(|entry| {
        !valid_command_id(&entry.command_id)
          || !ids.insert(&entry.command_id)
          || entry
            .keybinding
            .as_ref()
            .is_some_and(|key| accelerator(key).is_err())
      })
    {
      return Err(error("Invalid or duplicate keyboard shortcut entries."));
    }
    Ok(())
  }
}

pub fn valid_command_id(id: &str) -> bool {
  !id.is_empty()
    && id.len() <= 128
    && id
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_'))
}

pub fn accelerator(key: &Keybinding) -> CommandResult<String> {
  let code = key.code.as_str();
  let key_name = match code {
    "BracketLeft" => "[",
    "BracketRight" => "]",
    "Comma" => ",",
    "Period" => ".",
    "Slash" => "/",
    "Backslash" => "\\",
    "Minus" => "-",
    "Equal" => "=",
    "ArrowUp" => "Up",
    "ArrowDown" => "Down",
    "ArrowLeft" => "Left",
    "ArrowRight" => "Right",
    "Escape" | "Enter" | "Space" | "Backspace" | "Delete" | "Home" | "End" | "PageUp"
    | "PageDown" => code,
    _ if code.strip_prefix("Key").is_some_and(|value| {
      value.len() == 1 && value.bytes().all(|byte| byte.is_ascii_uppercase())
    }) =>
    {
      &code[3..]
    }
    _ if code
      .strip_prefix("Digit")
      .is_some_and(|value| value.len() == 1 && value.bytes().all(|byte| byte.is_ascii_digit())) =>
    {
      &code[5..]
    }
    _ if (1..=12).any(|number| code == format!("F{number}")) => code,
    _ => return Err(error("Unsupported shortcut key code.")),
  };
  Ok(format!(
    "{}{}{}{key_name}",
    if key.primary { "CmdOrCtrl+" } else { "" },
    if key.alt { "Alt+" } else { "" },
    if key.shift { "Shift+" } else { "" }
  ))
}

#[derive(Debug, Serialize)]
pub struct KeybindingsSnapshot {
  pub path: String,
  pub revision: Option<String>,
  pub document: KeybindingsDocument,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveKeybindingsRequest {
  pub expected_revision: Option<String>,
  pub document: KeybindingsDocument,
}

#[tauri::command]
pub async fn load_keybindings(app: tauri::AppHandle) -> CommandResult<KeybindingsSnapshot> {
  let directory = app
    .path()
    .app_config_dir()
    .map_err(CommandErrorDto::backend)?;
  tauri::async_runtime::spawn_blocking(move || read(&directory))
    .await
    .map_err(CommandErrorDto::backend)?
}

#[tauri::command]
pub async fn save_keybindings(
  app: tauri::AppHandle,
  request: SaveKeybindingsRequest,
) -> CommandResult<KeybindingsSnapshot> {
  let directory = app
    .path()
    .app_config_dir()
    .map_err(CommandErrorDto::backend)?;
  tauri::async_runtime::spawn_blocking(move || save(&directory, &request))
    .await
    .map_err(CommandErrorDto::backend)?
}

fn read(directory: &Path) -> CommandResult<KeybindingsSnapshot> {
  let path = directory.join("keybindings.json");
  regular_or_absent(&path)?;
  let source = match File::open(&path) {
    Ok(file) => {
      let mut source = String::new();
      file
        .take(MAX_BYTES + 1)
        .read_to_string(&mut source)
        .map_err(CommandErrorDto::backend)?;
      if source.len() as u64 > MAX_BYTES {
        return Err(error("keybindings.json is too large."));
      }
      Some(source)
    }
    Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => None,
    Err(failure) => return Err(CommandErrorDto::backend(failure)),
  };
  let document: KeybindingsDocument = source.as_ref().map_or_else(
    || Ok(KeybindingsDocument::default()),
    |source| {
      serde_json::from_str(source).map_err(|failure| {
        error(format!(
          "Could not read keybindings.json; the file is preserved: {failure}"
        ))
      })
    },
  )?;
  document.validate()?;
  Ok(KeybindingsSnapshot {
    path: path.to_string_lossy().into_owned(),
    revision: source,
    document,
  })
}

fn save(directory: &Path, request: &SaveKeybindingsRequest) -> CommandResult<KeybindingsSnapshot> {
  request.document.validate()?;
  let mut builder = fs::DirBuilder::new();
  builder.recursive(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  builder
    .create(directory)
    .map_err(CommandErrorDto::backend)?;
  let lock_path = directory.join("keybindings.lock");
  regular_or_absent(&lock_path)?;
  let lock = private_options()
    .create(true)
    .read(true)
    .write(true)
    .open(lock_path)
    .map_err(CommandErrorDto::backend)?;
  lock.lock().map_err(CommandErrorDto::backend)?;
  let current = read(directory)?;
  if current.revision != request.expected_revision {
    return Err(error(
      "Keyboard shortcuts changed on disk. Reload them before saving.",
    ));
  }
  let source = serde_json::to_string_pretty(&request.document).map_err(CommandErrorDto::backend)?;
  if source.len() as u64 > MAX_BYTES {
    return Err(error("Keyboard shortcuts exceed the size limit."));
  }
  let temporary =
    TemporaryFile(directory.join(format!(".keybindings-{}.tmp", uuid::Uuid::new_v4())));
  let mut file = private_options()
    .create_new(true)
    .write(true)
    .open(&temporary.0)
    .map_err(CommandErrorDto::backend)?;
  file
    .write_all(source.as_bytes())
    .map_err(CommandErrorDto::backend)?;
  file.sync_all().map_err(CommandErrorDto::backend)?;
  drop(file);
  fs::rename(&temporary.0, directory.join("keybindings.json")).map_err(CommandErrorDto::backend)?;
  #[cfg(unix)]
  File::open(directory)
    .and_then(|file| file.sync_all())
    .map_err(CommandErrorDto::backend)?;
  read(directory)
}

fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  options
}

fn regular_or_absent(path: &Path) -> CommandResult<()> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if metadata.is_file() => Ok(()),
    Ok(_) => Err(error(
      "Shortcut settings must be regular files, not symlinks or directories.",
    )),
    Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(failure) => Err(CommandErrorDto::backend(failure)),
  }
}

struct TemporaryFile(PathBuf);
impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ignored = fs::remove_file(&self.0);
  }
}

fn error(message: impl Into<String>) -> CommandErrorDto {
  CommandErrorDto::new("keybindings_invalid", message)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn editable_settings_round_trip_and_reject_stale_writes() {
    let dir = std::env::temp_dir().join(format!("rmux-keybindings-{}", uuid::Uuid::new_v4()));
    let initial = read(&dir).unwrap();
    assert!(initial.revision.is_none());
    let document = KeybindingsDocument {
      schema_version: 1,
      overrides: vec![KeybindingOverride {
        command_id: "session.close".into(),
        keybinding: None,
      }],
    };
    let saved = save(
      &dir,
      &SaveKeybindingsRequest {
        expected_revision: None,
        document: document.clone(),
      },
    )
    .unwrap();
    assert_eq!(saved.document, document);
    assert!(
      save(
        &dir,
        &SaveKeybindingsRequest {
          expected_revision: None,
          document: document.clone()
        }
      )
      .is_err()
    );
    fs::write(dir.join("keybindings.json"), "corrupt").unwrap();
    assert!(
      save(
        &dir,
        &SaveKeybindingsRequest {
          expected_revision: saved.revision,
          document
        }
      )
      .is_err()
    );
    assert_eq!(
      fs::read_to_string(dir.join("keybindings.json")).unwrap(),
      "corrupt"
    );
    fs::remove_dir_all(dir).unwrap();
  }

  #[test]
  fn accelerators_use_the_structured_keymap() {
    assert_eq!(
      accelerator(&Keybinding {
        code: "KeyE".into(),
        primary: true,
        shift: true,
        alt: false
      })
      .unwrap(),
      "CmdOrCtrl+Shift+E"
    );
    assert!(
      accelerator(&Keybinding {
        code: "arbitrary+command".into(),
        primary: true,
        shift: false,
        alt: false
      })
      .is_err()
    );
  }

  #[test]
  fn rejects_unknown_fields_missing_bindings_and_future_documents() {
    assert!(
      serde_json::from_str::<KeybindingsDocument>(
        r#"{"schema_version":1,"overrides":[{"command_id":"session.close"}]}"#
      )
      .is_err()
    );
    assert!(
      serde_json::from_str::<KeybindingsDocument>(
        r#"{"schema_version":1,"overrides":[],"unknown":true}"#
      )
      .is_err()
    );
    assert!(
      KeybindingsDocument {
        schema_version: 2,
        overrides: Vec::new()
      }
      .validate()
      .is_err()
    );
  }

  #[cfg(unix)]
  #[test]
  fn refuses_symlink_settings_without_changing_the_target() {
    let dir = std::env::temp_dir().join(format!("rmux-keybindings-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&dir).unwrap();
    let target = dir.join("unrelated.json");
    fs::write(&target, "keep me").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("keybindings.json")).unwrap();
    assert!(read(&dir).is_err());
    assert!(
      save(
        &dir,
        &SaveKeybindingsRequest {
          expected_revision: None,
          document: KeybindingsDocument::default()
        }
      )
      .is_err()
    );
    assert_eq!(fs::read_to_string(target).unwrap(), "keep me");
    fs::remove_dir_all(dir).unwrap();
  }
}
