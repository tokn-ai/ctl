//! Identity suggestions are directory metadata, never parsed private keys.
use std::fs;
use std::path::Path;

use crate::dto::{SshIdentityFileCatalogDto, SshIdentityFileDto};

pub fn discover_identity_files() -> SshIdentityFileCatalogDto {
  match dirs::home_dir() {
    Some(home) => discover_from_home(&home),
    None => SshIdentityFileCatalogDto {
      identity_files: Vec::new(),
      warnings: vec!["Could not locate ~/.ssh. Enter an identity-file path manually.".into()],
    },
  }
}

fn discover_from_home(home: &Path) -> SshIdentityFileCatalogDto {
  let mut catalog = SshIdentityFileCatalogDto::default();
  let entries = match fs::read_dir(home.join(".ssh")) {
    Ok(entries) => entries,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return catalog,
    Err(error) => {
      catalog.warnings.push(format!(
        "Could not list ~/.ssh: {error}. Enter a path manually."
      ));
      return catalog;
    }
  };
  for entry in entries {
    let entry = match entry {
      Ok(entry) => entry,
      Err(error) => {
        catalog
          .warnings
          .push(format!("Could not inspect a ~/.ssh entry: {error}"));
        continue;
      }
    };
    let filename = entry.file_name();
    let Some(filename) = filename.to_str().filter(|name| is_candidate_name(name)) else {
      continue;
    };
    let path = entry.path();
    // Follow key symlinks, but only stat their targets. Never open file contents.
    match fs::metadata(&path) {
      Ok(metadata) if metadata.is_file() => {
        if let Some(path) = path.to_str() {
          catalog.identity_files.push(SshIdentityFileDto {
            path: path.into(),
            display_path: format!("~/.ssh/{filename}"),
          });
        }
      }
      Ok(_) => {}
      Err(error) => catalog
        .warnings
        .push(format!("Could not inspect ~/.ssh/{filename}: {error}")),
    }
  }
  catalog
    .identity_files
    .sort_by(|left, right| left.display_path.cmp(&right.display_path));
  catalog
}

fn is_candidate_name(name: &str) -> bool {
  let lower = name.to_ascii_lowercase();
  !name.starts_with('.')
    && !name.ends_with('~')
    && !name.chars().any(char::is_control)
    && ![".pub", ".bak", ".old", ".tmp", ".swp", ".swo"]
      .iter()
      .any(|suffix| lower.ends_with(suffix))
    && ![
      "config",
      "known_hosts",
      "known_hosts2",
      "authorized_keys",
      "authorized_keys2",
      "environment",
      "rc",
    ]
    .iter()
    .any(|base| {
      lower == *base
        || lower
          .strip_prefix(base)
          .is_some_and(|suffix| suffix.starts_with('.'))
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  struct TestHome(std::path::PathBuf);

  impl TestHome {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("rmux-identity-test-{}", uuid::Uuid::new_v4()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }
  }

  impl Drop for TestHome {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  #[test]
  fn lists_sorted_candidate_paths_without_requiring_key_contents() {
    let home = TestHome::new();
    let ssh = home.0.join(".ssh");
    fs::create_dir(&ssh).unwrap();
    for name in [
      "local.id_rsa",
      "id_ed25519",
      "custom-key.pem",
      "config",
      "config.work",
      "known_hosts",
      "known_hosts2",
      "authorized_keys",
      "environment",
      "rc",
      "local.id_rsa.pub",
      "local.id_rsa~",
      "key.bak",
      ".DS_Store",
    ] {
      fs::write(ssh.join(name), []).unwrap();
    }
    fs::create_dir(ssh.join("key-directory")).unwrap();
    fs::write(ssh.join("key-directory/nested-key"), []).unwrap();
    let catalog = discover_from_home(&home.0);
    assert!(catalog.warnings.is_empty());
    assert_eq!(
      catalog
        .identity_files
        .iter()
        .map(|file| file.display_path.as_str())
        .collect::<Vec<_>>(),
      [
        "~/.ssh/custom-key.pem",
        "~/.ssh/id_ed25519",
        "~/.ssh/local.id_rsa"
      ]
    );
    assert!(
      catalog
        .identity_files
        .iter()
        .all(|file| Path::new(&file.path).is_absolute())
    );
  }

  #[test]
  fn missing_directory_is_empty_and_unreadable_directory_is_a_warning() {
    let home = TestHome::new();
    assert_eq!(
      discover_from_home(&home.0),
      SshIdentityFileCatalogDto::default()
    );
    fs::write(home.0.join(".ssh"), []).unwrap();
    let catalog = discover_from_home(&home.0);
    assert!(catalog.identity_files.is_empty());
    assert_eq!(catalog.warnings.len(), 1);
  }

  #[cfg(unix)]
  #[test]
  fn follows_file_symlinks_without_reading_keys_and_skips_directory_links() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let home = TestHome::new();
    let ssh = home.0.join(".ssh");
    fs::create_dir(&ssh).unwrap();
    let key = home.0.join("unreadable-key");
    fs::write(&key, []).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o000)).unwrap();
    symlink(&key, ssh.join("linked-key")).unwrap();
    symlink(&home.0, ssh.join("directory-link")).unwrap();
    symlink(home.0.join("missing"), ssh.join("broken-key")).unwrap();
    let catalog = discover_from_home(&home.0);
    assert_eq!(catalog.identity_files.len(), 1);
    assert_eq!(catalog.identity_files[0].display_path, "~/.ssh/linked-key");
    assert_eq!(catalog.warnings.len(), 1);
  }
}
