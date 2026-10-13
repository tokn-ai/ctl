//! Client-local preferences shared by every terminal UI launcher.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Terminal UI preferences. Omitted fields use their defaults.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
  pub selection: SelectionSettings,
}

/// Selection preferences apply to live pane history and archived output alike.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionSettings {
  pub mouse_release: MouseRelease,
  pub after_copy: AfterCopy,
}

/// What releasing a completed mouse drag does.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MouseRelease {
  /// Complete the selection without changing either clipboard.
  #[default]
  Select,
  /// Copy, then apply the configured `after_copy` behavior.
  Copy,
}

/// What happens after either an explicit copy key or a configured mouse copy.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AfterCopy {
  /// Preserve the highlight and scroll position.
  #[default]
  KeepSelection,
  /// Clear the highlight while preserving the scroll position.
  ClearSelection,
  /// Return a live pane to its output, or close the archive copy view.
  ExitCopyMode,
}

impl Settings {
  /// Read `~/.tokn/ctl/ctmux.json`, or the file named by `CTMUX_CONFIG_FILE`.
  /// A missing default file uses defaults; an explicit override must exist.
  /// This never creates or rewrites the configuration file.
  ///
  /// # Errors
  /// Returns an error identifying an unreadable, oversized, or invalid configuration.
  pub fn load() -> crate::Result<Self> {
    let (path, optional) = match std::env::var_os("CTMUX_CONFIG_FILE") {
      Some(path) if path.is_empty() => return Err("CTMUX_CONFIG_FILE must name a file".into()),
      Some(path) => (PathBuf::from(path), false),
      None => (ctl_core::paths::directory()?.join("ctmux.json"), true),
    };
    Self::read(&path, optional)
      .map_err(|error| format!("could not load ctmux settings {}: {error}", path.display()).into())
  }

  fn read(path: &Path, optional: bool) -> crate::Result<Self> {
    let metadata = match fs::metadata(path) {
      Ok(metadata) => metadata,
      Err(error) if optional && error.kind() == io::ErrorKind::NotFound => {
        return Ok(Self::default());
      }
      Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
      return Err("settings must be a regular file".into());
    }
    if metadata.len() > MAX_CONFIG_BYTES {
      return Err("settings exceed the 64 KiB size limit".into());
    }
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
      return Err("settings exceed the 64 KiB size limit".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  struct Fixture(PathBuf);

  impl Fixture {
    fn new() -> Self {
      let directory = std::env::temp_dir().join(format!("ctmux-settings-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&directory).unwrap();
      Self(directory)
    }

    fn path(&self) -> PathBuf {
      self.0.join("ctmux.json")
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn omitted_preferences_keep_the_select_only_default() {
    for json in ["{}", r#"{"selection":{}}"#] {
      assert_eq!(
        serde_json::from_str::<Settings>(json).unwrap(),
        Settings::default()
      );
    }
    let settings: Settings =
      serde_json::from_str(r#"{"selection":{"mouse_release":"copy"}}"#).unwrap();
    assert_eq!(settings.selection.mouse_release, MouseRelease::Copy);
    assert_eq!(settings.selection.after_copy, AfterCopy::KeepSelection);
  }

  #[test]
  fn parses_each_supported_copy_policy() {
    for (name, expected) in [
      ("keep-selection", AfterCopy::KeepSelection),
      ("clear-selection", AfterCopy::ClearSelection),
      ("exit-copy-mode", AfterCopy::ExitCopyMode),
    ] {
      let json = format!(r#"{{"selection":{{"after_copy":"{name}"}}}}"#);
      let settings: Settings = serde_json::from_str(&json).unwrap();
      assert_eq!(settings.selection.after_copy, expected);
      assert_eq!(settings.selection.mouse_release, MouseRelease::Select);
    }
  }

  #[test]
  fn rejects_typos_and_wrong_types_instead_of_silently_changing_behavior() {
    for json in [
      r#"{"selections":{}}"#,
      r#"{"selection":{"mouse_releas":"copy"}}"#,
      r#"{"selection":{"mouse_release":"paste"}}"#,
      r#"{"selection":{"after_copy":"exit"}}"#,
      r#"{"selection":null}"#,
      r#"{"selection":{"mouse_release":true}}"#,
      r#"{"selection":{"mouse_release":"copy","mouse_release":"select"}}"#,
    ] {
      assert!(serde_json::from_str::<Settings>(json).is_err(), "{json}");
    }
  }

  #[test]
  fn only_a_missing_default_file_is_optional() {
    let fixture = Fixture::new();
    assert_eq!(
      Settings::read(&fixture.path(), true).unwrap(),
      Settings::default()
    );
    assert!(Settings::read(&fixture.path(), false).is_err());
    assert!(Settings::read(&fixture.0, true).is_err());
  }

  #[test]
  fn reads_configuration_and_preserves_invalid_files() {
    let fixture = Fixture::new();
    let path = fixture.path();
    fs::write(
      &path,
      r#"{"selection":{"mouse_release":"copy","after_copy":"exit-copy-mode"}}"#,
    )
    .unwrap();
    let settings = Settings::read(&path, false).unwrap();
    assert_eq!(settings.selection.mouse_release, MouseRelease::Copy);
    assert_eq!(settings.selection.after_copy, AfterCopy::ExitCopyMode);
    for content in [
      b"not json".to_vec(),
      vec![b' '; usize::try_from(MAX_CONFIG_BYTES).unwrap() + 1],
    ] {
      fs::write(&path, &content).unwrap();
      assert!(Settings::read(&path, true).is_err());
      assert_eq!(fs::read(&path).unwrap(), content);
    }
  }
}
