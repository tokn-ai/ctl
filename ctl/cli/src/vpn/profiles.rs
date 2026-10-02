//! Private saved profiles shared by VPN commands and host VPN routes.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use ctl_client::hosts::SavedVpnDocument;
use ctl_ipc::VpnConnection;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) fn path() -> Result<PathBuf, Error> {
  std::env::var_os("CTL_VPNS_PATH").map_or_else(
    || {
      ctl_core::paths::directory()
        .map(|directory| directory.join("vpns.json"))
        .map_err(|_| Error::HomeUnavailable)
    },
    |path| Ok(PathBuf::from(path)),
  )
}

pub(crate) fn load(path: &Path) -> Result<SavedVpnDocument, Error> {
  match fs::symlink_metadata(path) {
    Ok(metadata) if !metadata.is_file() => return Err(Error::UnsafeFile),
    Ok(_) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      return Ok(SavedVpnDocument::default());
    }
    Err(_) => return Err(Error::Unreadable),
  }
  let file = private_options()
    .read(true)
    .open(path)
    .map_err(|_| Error::Unreadable)?;
  let metadata = file.metadata().map_err(|_| Error::Unreadable)?;
  require_private_file(&metadata)?;
  let mut bytes = zeroize::Zeroizing::new(Vec::new());
  file
    .take(MAX_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| Error::Unreadable)?;
  if bytes.len() as u64 > MAX_BYTES {
    return Err(Error::TooLarge);
  }
  // JSON parser diagnostics can contain passwords; expose only a fixed error.
  let document: SavedVpnDocument = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
  document.validate()?;
  Ok(document)
}

/// Append one new profile under the same writer lock used by the desktop.
/// Prompting happens before this call; the catalog is reread only after locking
/// so unrelated edits made while the questionnaire was open are preserved.
pub(crate) fn create(path: &Path, connection: &VpnConnection) -> Result<(), Error> {
  connection
    .validate()
    .map_err(|_| Error::InvalidConnection)?;
  let directory = path
    .parent()
    .filter(|parent| !parent.as_os_str().is_empty())
    .unwrap_or_else(|| Path::new("."));
  prepare_directory(directory)?;
  let _lock = lock_directory(directory)?;
  let mut document = load(path)?;
  if document
    .connections
    .iter()
    .any(|saved| saved.connection_id == connection.connection_id)
  {
    return Err(Error::DuplicateId);
  }
  if document
    .connections
    .iter()
    .any(|saved| saved.name == connection.name)
  {
    return Err(Error::DuplicateName);
  }
  document.schema_version = 2;
  document.connections.push(connection.clone());
  document.validate()?;
  let bytes = zeroize::Zeroizing::new(
    serde_json::to_vec_pretty(&document).map_err(|_| Error::EncodingFailed)?,
  );
  if bytes.len() as u64 > MAX_BYTES {
    return Err(Error::TooLarge);
  }
  let temporary = TemporaryFile(directory.join(format!(".vpns-{}.tmp", uuid::Uuid::new_v4())));
  let mut file = private_options()
    .create_new(true)
    .write(true)
    .open(&temporary.0)
    .map_err(|_| Error::WriteFailed)?;
  require_private_file(&file.metadata().map_err(|_| Error::WriteFailed)?)?;
  file.write_all(&bytes).map_err(|_| Error::WriteFailed)?;
  file.sync_all().map_err(|_| Error::WriteFailed)?;
  drop(file);
  require_private_file_or_absent(path)?;
  fs::rename(&temporary.0, path).map_err(|_| Error::WriteFailed)?;
  #[cfg(unix)]
  File::open(directory)
    .and_then(|file| file.sync_all())
    .map_err(|_| Error::WriteFailed)?;
  Ok(())
}

fn prepare_directory(directory: &Path) -> Result<(), Error> {
  match fs::symlink_metadata(directory) {
    Ok(_) => {}
    Err(error) if error.kind() == io::ErrorKind::NotFound => {
      let mut builder = fs::DirBuilder::new();
      builder.recursive(true);
      #[cfg(unix)]
      {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
      }
      builder.create(directory).map_err(|_| Error::WriteFailed)?;
    }
    Err(_) => return Err(Error::WriteFailed),
  }
  let metadata = fs::symlink_metadata(directory).map_err(|_| Error::WriteFailed)?;
  if !metadata.is_dir() || !safe_directory_permissions(&metadata) {
    return Err(Error::UnsafeDirectory);
  }
  Ok(())
}

fn lock_directory(directory: &Path) -> Result<File, Error> {
  // Desktop VPN mutations use this exact name, independently of the host and
  // workspace catalogs. Retain the descriptor until after the atomic commit.
  let path = directory.join("vpns.lock");
  require_private_file_or_absent(&path)?;
  let file = private_options()
    .create(true)
    .read(true)
    .write(true)
    .open(path)
    .map_err(|_| Error::WriteFailed)?;
  require_private_file(&file.metadata().map_err(|_| Error::WriteFailed)?)?;
  file.lock().map_err(|_| Error::WriteFailed)?;
  Ok(file)
}

fn private_options() -> OpenOptions {
  let mut options = OpenOptions::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600).custom_flags(i32::from_ne_bytes(
      (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
        .bits()
        .to_ne_bytes(),
    ));
  }
  #[cfg(windows)]
  {
    use std::os::windows::fs::OpenOptionsExt as _;
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
  }
  options
}

fn require_private_file_or_absent(path: &Path) -> Result<(), Error> {
  match fs::symlink_metadata(path) {
    Ok(metadata) => require_private_file(&metadata),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(_) => Err(Error::Unreadable),
  }
}

fn require_private_file(metadata: &Metadata) -> Result<(), Error> {
  if !metadata.is_file() || !owned_and_private(metadata) {
    return Err(Error::UnsafeFile);
  }
  Ok(())
}

fn owned_and_private(metadata: &Metadata) -> bool {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    let public_permissions = u32::from((rustix::fs::Mode::RWXG | rustix::fs::Mode::RWXO).bits());
    metadata.mode() & public_permissions == 0
      && metadata.uid() == rustix::process::getuid().as_raw()
  }
  #[cfg(not(unix))]
  {
    let _ = metadata;
    true
  }
}

fn safe_directory_permissions(metadata: &Metadata) -> bool {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    metadata.mode() & 0o022 == 0 && metadata.uid() == rustix::process::getuid().as_raw()
  }
  #[cfg(not(unix))]
  {
    let _ = metadata;
    true
  }
}

struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.0);
  }
}

pub(super) fn resolve(
  mut document: SavedVpnDocument,
  selector: &str,
) -> Result<VpnConnection, Error> {
  let index = if let Some(index) = document
    .connections
    .iter()
    .position(|connection| connection.connection_id == selector)
  {
    index
  } else {
    let mut matches = document
      .connections
      .iter()
      .enumerate()
      .filter(|(_, connection)| connection.name == selector);
    let Some((index, _)) = matches.next() else {
      return Err(Error::NotFound(selector.into()));
    };
    if matches.next().is_some() {
      return Err(Error::Ambiguous(selector.into()));
    }
    index
  };
  Ok(document.connections.swap_remove(index))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("Could not locate the home directory for saved VPN profiles.")]
  HomeUnavailable,
  #[error("Could not read saved VPN profiles; the file has been preserved.")]
  Unreadable,
  #[error("VPN settings must be a regular file owned by the current user and private (mode 0600).")]
  UnsafeFile,
  #[error(
    "The VPN settings directory must be owned by the current user, not writable by other users, and not a symlink."
  )]
  UnsafeDirectory,
  #[error("Saved VPN profiles exceed the size limit; the file has been preserved.")]
  TooLarge,
  #[error("Could not read vpns.json; the file has been preserved.")]
  Invalid,
  #[error("The new VPN profile has invalid settings; the saved catalog has not been changed.")]
  InvalidConnection,
  #[error("A saved VPN already uses this connection ID; the saved catalog has not been changed.")]
  DuplicateId,
  #[error("A saved VPN already uses this name; choose another name.")]
  DuplicateName,
  #[error("Could not encode the VPN profiles; the saved catalog has not been changed.")]
  EncodingFailed,
  #[error("Could not finish saving the VPN profiles; reload the catalog before retrying.")]
  WriteFailed,
  #[error(transparent)]
  Validation(#[from] ctl_client::hosts::HostError),
  #[error("No saved VPN matches {0:?}. Use `ctl vpn list` to see saved profiles.")]
  NotFound(String),
  #[error("More than one saved VPN matches {0:?}; use its VPN ID from `ctl vpn list`.")]
  Ambiguous(String),
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use ctl_ipc::VpnSettings;
  use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
  use std::sync::{Arc, Barrier, mpsc};
  use std::time::Duration;

  struct Fixture(PathBuf);

  impl Fixture {
    fn new() -> Self {
      let root = std::env::temp_dir().join(format!("ctl-vpn-profiles-{}", uuid::Uuid::new_v4()));
      fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
      Self(root)
    }

    fn path(&self) -> PathBuf {
      self.0.join("vpns.json")
    }

    fn write(&self, bytes: &[u8]) {
      fs::write(self.path(), bytes).unwrap();
      fs::set_permissions(self.path(), fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn write_document(&self, connections: Vec<VpnConnection>) {
      self.write(
        &serde_json::to_vec_pretty(&SavedVpnDocument {
          connections,
          ..SavedVpnDocument::default()
        })
        .unwrap(),
      );
    }

    fn assert_no_temporary_files(&self) {
      assert!(fs::read_dir(&self.0).unwrap().all(|entry| {
        !entry
          .unwrap()
          .file_name()
          .to_string_lossy()
          .starts_with(".vpns-")
      }));
    }
  }

  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  fn profile(id: &str) -> VpnConnection {
    VpnConnection {
      connection_id: id.into(),
      name: format!("Profile {id}"),
      settings: VpnSettings::Openconnect {
        url: "https://vpn.example.test/private?token=fixture-token".into(),
        username: "fixture-user".into(),
        password: zeroize::Zeroizing::new("fixture-password".into()),
        auth_method: Some("fixture-group".into()),
        target_ip: Some("192.0.2.1".into()),
      },
    }
  }

  #[test]
  fn creates_private_catalog_and_lock_in_new_private_directories() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("new/nested");
    let path = directory.join("custom.json");
    let connection = profile("work");
    create(&path, &connection).unwrap();
    let saved = load(&path).unwrap();
    assert_eq!(saved.schema_version, 2);
    assert_eq!(saved.connections.len(), 1);
    assert!(saved.connections[0] == connection);
    for path in [&directory, &fixture.0.join("new")] {
      assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
      );
    }
    for path in [&path, &directory.join("vpns.lock")] {
      assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
      );
    }
    assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
  }

  #[test]
  fn accepts_owned_readable_parent_without_changing_its_permissions() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
    create(&fixture.path(), &profile("work")).unwrap();
    assert_eq!(load(&fixture.path()).unwrap().connections.len(), 1);
    assert_eq!(
      fs::metadata(&fixture.0).unwrap().permissions().mode() & 0o777,
      0o755
    );
    assert_eq!(
      fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
      0o600
    );
  }

  #[test]
  fn migrates_legacy_catalog_only_when_appending_and_preserves_existing_settings() {
    let fixture = Fixture::new();
    let legacy = serde_json::json!({
      "schema_version": 1,
      "connections": [{
        "connection_id": "legacy",
        "name": "Legacy profile",
        "url": "https://vpn.example.test/group?token=legacy-token",
        "username": "legacy-user",
        "password": "legacy-password",
        "auth_method": "legacy-group",
        "target_ip": "192.0.2.2"
      }]
    });
    let original = serde_json::to_vec(&legacy).unwrap();
    fixture.write(&original);
    let before = load(&fixture.path()).unwrap();
    assert_eq!(before.schema_version, 1);
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    let tailscale = VpnConnection {
      connection_id: "tailnet".into(),
      name: "Tailnet".into(),
      settings: VpnSettings::Tailscale {
        hostname: Some("fixture-device".into()),
        accept_routes: true,
      },
    };
    create(&fixture.path(), &tailscale).unwrap();
    let after = load(&fixture.path()).unwrap();
    assert_eq!(after.schema_version, 2);
    assert_eq!(after.connections.len(), 2);
    assert!(after.connections[0] == before.connections[0]);
    assert!(after.connections[1] == tailscale);
    fixture.assert_no_temporary_files();
  }

  #[test]
  fn rejects_duplicate_ids_and_new_duplicate_names_without_rewriting() {
    let fixture = Fixture::new();
    fixture.write_document(vec![profile("work")]);
    let original = fs::read(fixture.path()).unwrap();
    let duplicate_id = VpnConnection {
      name: "Different name".into(),
      ..profile("work")
    };
    assert!(matches!(
      create(&fixture.path(), &duplicate_id),
      Err(Error::DuplicateId)
    ));
    let duplicate_name = VpnConnection {
      name: "Profile work".into(),
      ..profile("another")
    };
    assert!(matches!(
      create(&fixture.path(), &duplicate_name),
      Err(Error::DuplicateName)
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    fixture.assert_no_temporary_files();
  }

  #[test]
  fn existing_duplicate_names_remain_readable_when_creating_a_unique_profile() {
    let fixture = Fixture::new();
    let first = profile("first");
    let second = VpnConnection {
      name: first.name.clone(),
      ..profile("second")
    };
    fixture.write_document(vec![first, second]);
    create(&fixture.path(), &profile("third")).unwrap();
    let saved = load(&fixture.path()).unwrap();
    assert_eq!(saved.connections.len(), 3);
    assert!(matches!(
      resolve(saved, "Profile first"),
      Err(Error::Ambiguous(_))
    ));
  }

  #[test]
  fn concurrent_creates_preserve_both_appended_profiles() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(3));
    let writers: Vec<_> = ["first", "second"]
      .into_iter()
      .map(|id| {
        let path = fixture.path();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
          barrier.wait();
          create(&path, &profile(id))
        })
      })
      .collect();
    barrier.wait();
    for writer in writers {
      writer.join().unwrap().unwrap();
    }
    let saved = load(&fixture.path()).unwrap();
    assert_eq!(saved.connections.len(), 2);
    for id in ["first", "second"] {
      assert!(
        saved
          .connections
          .iter()
          .any(|profile| profile.connection_id == id)
      );
    }
    fixture.assert_no_temporary_files();
  }

  #[test]
  fn rereads_after_acquiring_the_desktop_lock_so_waiting_creates_preserve_edits() {
    let fixture = Fixture::new();
    fixture.write_document(vec![profile("existing")]);
    let lock = private_options()
      .create(true)
      .read(true)
      .write(true)
      .open(fixture.0.join("vpns.lock"))
      .unwrap();
    lock.lock().unwrap();
    let path = fixture.path();
    let (started_sender, started_receiver) = mpsc::channel();
    let (completed_sender, completed_receiver) = mpsc::channel();
    let writer = std::thread::spawn(move || {
      started_sender.send(()).unwrap();
      completed_sender
        .send(create(&path, &profile("created")))
        .unwrap();
    });
    started_receiver.recv().unwrap();
    assert!(matches!(
      completed_receiver.recv_timeout(Duration::from_millis(50)),
      Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let updated = VpnConnection {
      name: "Updated in desktop".into(),
      ..profile("existing")
    };
    fixture.write_document(vec![updated.clone(), profile("desktop-added")]);
    drop(lock);
    completed_receiver
      .recv_timeout(Duration::from_secs(5))
      .unwrap()
      .unwrap();
    writer.join().unwrap();
    let saved = load(&fixture.path()).unwrap();
    assert_eq!(saved.connections.len(), 3);
    assert!(saved.connections[0] == updated);
    assert_eq!(saved.connections[1].connection_id, "desktop-added");
    assert_eq!(saved.connections[2].connection_id, "created");
  }

  #[test]
  fn rejects_unsafe_parent_permissions_without_changing_them() {
    let fixture = Fixture::new();
    for mode in [0o777, 0o775] {
      fs::set_permissions(&fixture.0, fs::Permissions::from_mode(mode)).unwrap();
      assert!(matches!(
        create(&fixture.path(), &profile("work")),
        Err(Error::UnsafeDirectory)
      ));
      assert!(!fixture.path().exists());
      assert!(!fixture.0.join("vpns.lock").exists());
      assert_eq!(
        fs::metadata(&fixture.0).unwrap().permissions().mode() & 0o777,
        mode
      );
    }
  }

  #[test]
  fn rejects_symlink_parents_catalogs_and_locks_without_touching_their_targets() {
    let fixture = Fixture::new();
    let actual = fixture.0.join("actual");
    fs::DirBuilder::new().mode(0o700).create(&actual).unwrap();
    let link = fixture.0.join("directory-link");
    symlink(&actual, &link).unwrap();
    assert!(matches!(
      create(&link.join("vpns.json"), &profile("work")),
      Err(Error::UnsafeDirectory)
    ));
    assert_eq!(fs::read_dir(&actual).unwrap().count(), 0);

    let target = fixture.0.join("target.json");
    fs::write(&target, b"private-target-marker").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, fixture.path()).unwrap();
    assert!(matches!(
      create(&fixture.path(), &profile("work")),
      Err(Error::UnsafeFile)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"private-target-marker");
    fs::remove_file(fixture.path()).unwrap();
    fs::remove_file(fixture.0.join("vpns.lock")).unwrap();
    symlink(&target, fixture.0.join("vpns.lock")).unwrap();
    assert!(matches!(
      create(&fixture.path(), &profile("work")),
      Err(Error::UnsafeFile)
    ));
    assert_eq!(fs::read(&target).unwrap(), b"private-target-marker");
    assert!(!fixture.path().exists());
    fixture.assert_no_temporary_files();
  }

  #[test]
  fn malformed_or_public_catalogs_are_preserved_with_secret_safe_errors() {
    let fixture = Fixture::new();
    let malformed = br#"{"schema_version": 2, "connections": ["private-password-marker"]}"#;
    fixture.write(malformed);
    let error = create(&fixture.path(), &profile("work")).unwrap_err();
    assert!(matches!(error, Error::Invalid));
    assert!(!error.to_string().contains("private-password-marker"));
    assert_eq!(fs::read(fixture.path()).unwrap(), malformed);

    fixture.write_document(vec![profile("existing")]);
    let original = fs::read(fixture.path()).unwrap();
    fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
      create(&fixture.path(), &profile("work")),
      Err(Error::UnsafeFile)
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(
      fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
      0o644
    );
    fixture.assert_no_temporary_files();
  }

  #[test]
  fn oversized_commit_preserves_the_previous_catalog() {
    let fixture = Fixture::new();
    let mut document = SavedVpnDocument::default();
    let incoming = loop {
      let mut candidate = profile(&format!("large-{}", document.connections.len()));
      let VpnSettings::Openconnect { password, .. } = &mut candidate.settings else {
        unreachable!();
      };
      *password = zeroize::Zeroizing::new("\u{1b}".repeat(4096));
      document.connections.push(candidate.clone());
      if serde_json::to_vec_pretty(&document).unwrap().len() as u64 > MAX_BYTES {
        document.connections.pop();
        break candidate;
      }
    };
    document.validate().unwrap();
    let original = serde_json::to_vec_pretty(&document).unwrap();
    fixture.write(&original);
    assert!(matches!(
      create(&fixture.path(), &incoming),
      Err(Error::TooLarge)
    ));
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    fixture.assert_no_temporary_files();
  }
}
