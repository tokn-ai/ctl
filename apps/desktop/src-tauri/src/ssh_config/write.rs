use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::{HomeDirectoryUnavailable, discover_hosts_from_home};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostDefinition {
  pub alias: String,
  pub hostname: String,
  pub user: Option<String>,
  pub port: Option<u16>,
  pub identity_file: Option<String>,
}

#[derive(Debug, Error)]
pub enum SaveSshConfigError {
  #[error("invalid SSH host field '{0}'")]
  InvalidField(&'static str),
  #[error("cannot safely check SSH host conflicts because discovery was incomplete: {0}")]
  IncompleteDiscovery(String),
  #[error("SSH alias '{0}' is already defined outside rmux-app's managed block")]
  AliasConflict(String),
  #[error("rmux-app's managed SSH block for '{0}' is malformed")]
  MalformedManagedBlock(String),
  #[error("SSH config changed while rmux-app was saving it; no changes were written")]
  ConcurrentModification,
  #[error("could not {action} SSH config at {}: {source}", path.display())]
  Io {
    action: &'static str,
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },
  #[error(transparent)]
  HomeDirectoryUnavailable(#[from] HomeDirectoryUnavailable),
}

/// Writes or updates one rmux-app-managed `Host` block in the user's OpenSSH
/// config. Existing unmanaged aliases are never overwritten.
pub fn save_host(definition: &SshHostDefinition) -> Result<String, SaveSshConfigError> {
  let home = dirs::home_dir().ok_or(HomeDirectoryUnavailable)?;
  save_host_to_home(&home, definition)
}

fn save_host_to_home(
  home: &Path,
  definition: &SshHostDefinition,
) -> Result<String, SaveSshConfigError> {
  validate_host_definition(definition)?;
  let discovery = discover_hosts_from_home(home);
  if let Some(warning) = discovery.warnings.first() {
    return Err(SaveSshConfigError::IncompleteDiscovery(warning.clone()));
  }

  let ssh_directory = home.join(".ssh");
  let directory_existed = ssh_directory.exists();
  fs::create_dir_all(&ssh_directory).map_err(|source| SaveSshConfigError::Io {
    action: "create the directory for",
    path: ssh_directory.clone(),
    source,
  })?;
  #[cfg(unix)]
  if !directory_existed {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&ssh_directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
      SaveSshConfigError::Io {
        action: "set permissions on the directory for",
        path: ssh_directory.clone(),
        source,
      }
    })?;
  }

  let configured_path = ssh_directory.join("config");
  let config_path = if configured_path.exists() {
    fs::canonicalize(&configured_path).map_err(|source| SaveSshConfigError::Io {
      action: "resolve",
      path: configured_path.clone(),
      source,
    })?
  } else {
    configured_path
  };
  let original = match fs::read_to_string(&config_path) {
    Ok(contents) => contents,
    Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
    Err(source) => {
      return Err(SaveSshConfigError::Io {
        action: "read",
        path: config_path,
        source,
      });
    }
  };
  let managed_range = managed_block_range(&original, &definition.alias)?;
  let occurrences = discovery
    .occurrences
    .get(&definition.alias)
    .copied()
    .unwrap_or_default();
  if (managed_range.is_none() && occurrences > 0) || (managed_range.is_some() && occurrences > 1) {
    return Err(SaveSshConfigError::AliasConflict(definition.alias.clone()));
  }

  let block = render_managed_block(definition);
  let updated = if let Some(range) = managed_range {
    format!(
      "{}{}{}",
      &original[..range.start],
      block,
      &original[range.end..]
    )
  } else {
    format!("{block}\nHost *\n\n{original}")
  };
  write_config_atomically(&config_path, original.as_bytes(), updated.as_bytes())?;
  Ok(definition.alias.clone())
}

fn validate_host_definition(definition: &SshHostDefinition) -> Result<(), SaveSshConfigError> {
  if !is_safe_config_token(&definition.alias)
    || definition.alias.starts_with('!')
    || definition.alias.contains(['*', '?'])
  {
    return Err(SaveSshConfigError::InvalidField("alias"));
  }
  if !is_safe_config_token(&definition.hostname) {
    return Err(SaveSshConfigError::InvalidField("hostname"));
  }
  if definition
    .user
    .as_ref()
    .is_some_and(|user| !is_safe_config_token(user))
  {
    return Err(SaveSshConfigError::InvalidField("user"));
  }
  if definition.port == Some(0) {
    return Err(SaveSshConfigError::InvalidField("port"));
  }
  if definition
    .identity_file
    .as_ref()
    .is_some_and(|path| path.trim().is_empty() || path.chars().any(char::is_control))
  {
    return Err(SaveSshConfigError::InvalidField("identity_file"));
  }
  Ok(())
}

fn is_safe_config_token(value: &str) -> bool {
  !value.is_empty()
    && !value.chars().any(|character| {
      character.is_whitespace()
        || character.is_control()
        || matches!(character, '#' | '\\' | '\'' | '"' | '=' | '%')
    })
}

fn render_managed_block(definition: &SshHostDefinition) -> String {
  let mut block = format!(
    "# >>> rmux-app host {}\nHost {}\n  HostName {}\n",
    definition.alias, definition.alias, definition.hostname
  );
  if let Some(user) = &definition.user {
    writeln!(block, "  User {user}").expect("writing to a String cannot fail");
  }
  if let Some(port) = definition.port {
    writeln!(block, "  Port {port}").expect("writing to a String cannot fail");
  }
  if let Some(identity_file) = &definition.identity_file {
    writeln!(
      block,
      "  IdentityFile \"{}\"",
      escape_config_string(identity_file)
    )
    .expect("writing to a String cannot fail");
    block.push_str("  IdentitiesOnly yes\n");
  }
  writeln!(block, "# <<< rmux-app host {}", definition.alias)
    .expect("writing to a String cannot fail");
  block
}

fn escape_config_string(value: &str) -> String {
  value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn managed_block_range(
  contents: &str,
  alias: &str,
) -> Result<Option<std::ops::Range<usize>>, SaveSshConfigError> {
  let begin_marker = format!("# >>> rmux-app host {alias}");
  let end_marker = format!("# <<< rmux-app host {alias}");
  let mut start = None;
  let mut offset = 0;
  for segment in contents.split_inclusive('\n') {
    let line = segment.trim_end_matches(['\r', '\n']);
    if line == begin_marker {
      if start.is_some() {
        return Err(SaveSshConfigError::MalformedManagedBlock(alias.into()));
      }
      start = Some(offset);
    } else if line == end_marker {
      let Some(start) = start.take() else {
        return Err(SaveSshConfigError::MalformedManagedBlock(alias.into()));
      };
      return Ok(Some(start..offset + segment.len()));
    }
    offset += segment.len();
  }
  if start.is_some() {
    return Err(SaveSshConfigError::MalformedManagedBlock(alias.into()));
  }
  Ok(None)
}

fn write_config_atomically(
  path: &Path,
  expected: &[u8],
  updated: &[u8],
) -> Result<(), SaveSshConfigError> {
  let parent = path.parent().ok_or_else(|| SaveSshConfigError::Io {
    action: "locate the parent directory of",
    path: path.to_path_buf(),
    source: std::io::Error::other("SSH config path has no parent directory"),
  })?;
  let temporary_path = parent.join(format!(".config.rmux-app-{}.tmp", uuid::Uuid::new_v4()));
  let mut temporary = OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&temporary_path)
    .map_err(|source| SaveSshConfigError::Io {
      action: "create a temporary file for",
      path: path.to_path_buf(),
      source,
    })?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    let permissions = match fs::metadata(path) {
      Ok(metadata) => metadata.permissions(),
      Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
        fs::Permissions::from_mode(0o600)
      }
      Err(source) => {
        let _ = fs::remove_file(&temporary_path);
        return Err(SaveSshConfigError::Io {
          action: "read permissions for",
          path: path.to_path_buf(),
          source,
        });
      }
    };
    if let Err(source) = fs::set_permissions(&temporary_path, permissions) {
      let _ = fs::remove_file(&temporary_path);
      return Err(SaveSshConfigError::Io {
        action: "set permissions on a temporary file for",
        path: path.to_path_buf(),
        source,
      });
    }
  }
  let result = (|| {
    temporary
      .write_all(updated)
      .and_then(|()| temporary.sync_all())
      .map_err(|source| SaveSshConfigError::Io {
        action: "write a temporary file for",
        path: path.to_path_buf(),
        source,
      })?;
    drop(temporary);
    let current = match fs::read(path) {
      Ok(contents) => contents,
      Err(source) if source.kind() == std::io::ErrorKind::NotFound => Vec::new(),
      Err(source) => {
        return Err(SaveSshConfigError::Io {
          action: "verify",
          path: path.to_path_buf(),
          source,
        });
      }
    };
    if current != expected {
      return Err(SaveSshConfigError::ConcurrentModification);
    }
    fs::rename(&temporary_path, path).map_err(|source| SaveSshConfigError::Io {
      action: "replace",
      path: path.to_path_buf(),
      source,
    })?;
    Ok(())
  })();
  if result.is_err() {
    let _ = fs::remove_file(temporary_path);
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn saves_and_updates_a_managed_host_before_existing_configuration() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    fs::create_dir_all(&ssh_directory).unwrap();
    fs::write(
      ssh_directory.join("config"),
      "# personal settings\nHost existing\n  HostName existing.example\n",
    )
    .unwrap();
    let mut definition = test_definition();

    assert_eq!(
      save_host_to_home(temporary.path(), &definition).unwrap(),
      "rmux-remote-test"
    );
    let first = fs::read_to_string(ssh_directory.join("config")).unwrap();
    assert!(first.starts_with("# >>> rmux-app host rmux-remote-test\n"));
    assert!(first.contains("  HostName 127.0.0.1\n"));
    assert!(first.contains("  Port 2222\n"));
    assert!(first.contains("  IdentityFile \"~/.ssh/local.id_rsa\"\n"));
    assert!(first.contains("# <<< rmux-app host rmux-remote-test\n\nHost *\n\n"));
    assert!(first.ends_with("  HostName existing.example\n"));

    definition.port = Some(2200);
    definition.user = Some("updated-user".into());
    save_host_to_home(temporary.path(), &definition).unwrap();
    let updated = fs::read_to_string(ssh_directory.join("config")).unwrap();
    assert_eq!(
      updated
        .matches("# >>> rmux-app host rmux-remote-test")
        .count(),
      1
    );
    assert!(updated.contains("  User updated-user\n"));
    assert!(updated.contains("  Port 2200\n"));
    assert!(!updated.contains("  Port 2222\n"));
    assert!(updated.ends_with("  HostName existing.example\n"));
  }

  #[test]
  fn refuses_to_overwrite_an_unmanaged_alias() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    fs::create_dir_all(&ssh_directory).unwrap();
    let config_path = ssh_directory.join("config");
    let original = "Host rmux-remote-test\n  HostName elsewhere\n";
    fs::write(&config_path, original).unwrap();

    let error = save_host_to_home(temporary.path(), &test_definition()).unwrap_err();

    assert!(matches!(error, SaveSshConfigError::AliasConflict(_)));
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
  }

  #[test]
  fn refuses_an_alias_declared_by_an_included_config() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    let include_directory = ssh_directory.join("config.d");
    fs::create_dir_all(&include_directory).unwrap();
    let config_path = ssh_directory.join("config");
    let original = "Include config.d/*.conf\n";
    fs::write(&config_path, original).unwrap();
    fs::write(
      include_directory.join("remote.conf"),
      "Host rmux-remote-test\n  HostName elsewhere\n",
    )
    .unwrap();

    let error = save_host_to_home(temporary.path(), &test_definition()).unwrap_err();

    assert!(matches!(error, SaveSshConfigError::AliasConflict(_)));
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
  }

  #[test]
  fn refuses_to_write_when_alias_discovery_is_incomplete() {
    let temporary = TemporaryDirectory::new();
    let ssh_directory = temporary.path().join(".ssh");
    fs::create_dir_all(&ssh_directory).unwrap();
    let config_path = ssh_directory.join("config");
    let original = "Include hosts/%h\n";
    fs::write(&config_path, original).unwrap();

    let error = save_host_to_home(temporary.path(), &test_definition()).unwrap_err();

    assert!(matches!(error, SaveSshConfigError::IncompleteDiscovery(_)));
    assert_eq!(fs::read_to_string(config_path).unwrap(), original);
  }

  #[cfg(unix)]
  #[test]
  fn creates_private_ssh_paths_for_a_new_configuration() {
    use std::os::unix::fs::PermissionsExt as _;

    let temporary = TemporaryDirectory::new();

    save_host_to_home(temporary.path(), &test_definition()).unwrap();

    let ssh_directory = temporary.path().join(".ssh");
    assert_eq!(
      fs::metadata(&ssh_directory).unwrap().permissions().mode() & 0o777,
      0o700
    );
    assert_eq!(
      fs::metadata(ssh_directory.join("config"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }

  #[test]
  fn rejects_invalid_fields_before_creating_ssh_configuration() {
    let temporary = TemporaryDirectory::new();
    let mut definition = test_definition();
    definition.alias = "host\ncommand".into();

    let error = save_host_to_home(temporary.path(), &definition).unwrap_err();

    assert!(matches!(error, SaveSshConfigError::InvalidField("alias")));
    assert!(!temporary.path().join(".ssh").exists());
  }

  fn test_definition() -> SshHostDefinition {
    SshHostDefinition {
      alias: "rmux-remote-test".into(),
      hostname: "127.0.0.1".into(),
      user: Some("rmux".into()),
      port: Some(2222),
      identity_file: Some("~/.ssh/local.id_rsa".into()),
    }
  }

  struct TemporaryDirectory(PathBuf);

  impl TemporaryDirectory {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("rmux-ssh-config-{}", uuid::Uuid::new_v4()));
      fs::create_dir(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }
}
