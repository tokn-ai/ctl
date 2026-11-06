//! Process-held leases make crash cleanup independent of PIDs and wall clocks.

use super::{ClientIdentity, ServiceConnection, valid_label};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RECORD_BYTES: u64 = 8192;
const MAX_RECORDS: usize = 4096;

/// Keeps a channel visible exactly while its gateway owns the lease.
pub struct ConnectionRegistration {
  lease: Option<File>,
  record_path: PathBuf,
  lease_path: PathBuf,
}

impl ConnectionRegistration {
  /// Registers a service channel without starting or contacting a daemon.
  ///
  /// # Errors
  /// Returns an error if labels are invalid or private state cannot be written.
  pub fn register(
    client: Option<ClientIdentity>,
    service: &str,
    source: Option<String>,
  ) -> io::Result<Self> {
    register_at(&directory()?, client, service, source)
  }
}

impl Drop for ConnectionRegistration {
  fn drop(&mut self) {
    let _ = fs::remove_file(&self.record_path);
    let _ = fs::remove_file(self.record_path.with_extension("tmp"));
    drop(self.lease.take());
    let _ = fs::remove_file(&self.lease_path);
    let _ = fs::remove_file(self.lease_path.with_extension("locktmp"));
  }
}

fn directory() -> io::Result<PathBuf> {
  Ok(crate::paths::directory()?.join("connections"))
}

fn private_directory(directory: &Path) -> io::Result<()> {
  fs::create_dir_all(directory)?;
  let info = directory_metadata(directory)?
    .ok_or_else(|| io::Error::other("Connection registry disappeared"))?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
  }
  let _ = info;
  Ok(())
}

fn directory_metadata(directory: &Path) -> io::Result<Option<fs::Metadata>> {
  let info = match fs::symlink_metadata(directory) {
    Ok(info) => info,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error),
  };
  if !info.is_dir() || info.file_type().is_symlink() {
    return Err(io::Error::other(
      "Connection registry must be a private directory",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if info.uid() != rustix::process::geteuid().as_raw() {
      return Err(io::Error::other(
        "Connection registry belongs to another account",
      ));
    }
  }
  Ok(Some(info))
}

fn open_existing(path: &Path, writable: bool) -> io::Result<File> {
  #[cfg(unix)]
  let file = {
    use rustix::fs::{Mode, OFlags, open};
    let access = if writable {
      OFlags::RDWR
    } else {
      OFlags::RDONLY
    };
    File::from(open(
      path,
      access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
      Mode::empty(),
    )?)
  };
  #[cfg(not(unix))]
  let file = {
    let info = fs::symlink_metadata(path)?;
    if !info.is_file() || info.file_type().is_symlink() {
      return Err(io::Error::other("Connection state must be a regular file"));
    }
    OpenOptions::new().read(true).write(writable).open(path)?
  };
  let info = file.metadata()?;
  if !info.is_file() {
    return Err(io::Error::other("Connection state must be a regular file"));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    if info.uid() != rustix::process::geteuid().as_raw() || info.permissions().mode() & 0o077 != 0 {
      return Err(io::Error::other(
        "Connection state must be private to this account",
      ));
    }
  }
  Ok(file)
}

fn create_private(path: &Path) -> io::Result<File> {
  let mut options = OpenOptions::new();
  options.read(true).write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
  }
  options.open(path)
}

fn register_at(
  directory: &Path,
  client: Option<ClientIdentity>,
  service: &str,
  source: Option<String>,
) -> io::Result<ConnectionRegistration> {
  if client.as_ref().is_some_and(|client| !client.validate())
    || !matches!(service, "ctmux" | "task" | "vpn")
    || source
      .as_deref()
      .is_some_and(|source| !valid_label(source, 128))
  {
    return Err(io::Error::new(
      io::ErrorKind::InvalidInput,
      "Invalid connection metadata",
    ));
  }
  private_directory(directory)?;
  let connection_id = uuid::Uuid::new_v4().to_string();
  let lease_path = directory.join(format!("{connection_id}.lease"));
  let record_path = directory.join(format!("{connection_id}.json"));
  let lease = create_private(&lease_path.with_extension("locktmp"))?;
  let registration = ConnectionRegistration {
    lease: Some(lease),
    record_path,
    lease_path,
  };
  registration
    .lease
    .as_ref()
    .expect("new registration owns its lease")
    .lock()?;
  // Readers must never see an unlocked lease between file creation and lock.
  fs::hard_link(
    registration.lease_path.with_extension("locktmp"),
    &registration.lease_path,
  )?;
  fs::remove_file(registration.lease_path.with_extension("locktmp"))?;
  let record = ServiceConnection {
    connection_id,
    client,
    service: service.into(),
    connected_at_ms: u64::try_from(
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis(),
    )
    .unwrap_or(u64::MAX),
    source,
  };
  let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
  let temporary = registration.record_path.with_extension("tmp");
  let result = (|| {
    let mut file = create_private(&temporary)?;
    file.write_all(&bytes)?;
    drop(file);
    // Publish only complete JSON, without replacing an existing record.
    fs::hard_link(&temporary, &registration.record_path)
  })();
  let _ = fs::remove_file(temporary);
  result?;
  Ok(registration)
}

/// Lists registered live inbound channels; missing state returns an empty list.
///
/// # Errors
/// Returns errors rather than reporting an incomplete registry as empty.
pub fn connections() -> io::Result<Vec<ServiceConnection>> {
  connections_in(&directory()?)
}

/// Reads an explicitly selected registry and removes unlocked stale records.
///
/// # Errors
/// Returns errors rather than presenting malformed or unreadable state as empty.
pub fn connections_in(directory: &Path) -> io::Result<Vec<ServiceConnection>> {
  let Some(info) = directory_metadata(directory)? else {
    return Ok(Vec::new());
  };
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    if info.permissions().mode() & 0o077 != 0 {
      return Err(io::Error::other(
        "Connection registry must be private to this account",
      ));
    }
  }
  let _ = info;
  let entries = match fs::read_dir(directory) {
    Ok(entries) => entries,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(error) => return Err(error),
  };
  let mut records = Vec::new();
  let mut checked = 0;
  for entry in entries {
    let entry = entry?;
    let path = entry.path();
    // Only published leases can be swept. A temporary lease can still be
    // between creation and locking, so probing it could race its publisher.
    if path
      .extension()
      .is_none_or(|extension| extension != "lease")
    {
      continue;
    }
    checked += 1;
    if checked > MAX_RECORDS {
      return Err(io::Error::other(
        "Connection registry exceeds its entry limit",
      ));
    }
    let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
      continue;
    };
    if uuid::Uuid::parse_str(id).is_err() || !entry.file_type()?.is_file() {
      return Err(io::Error::other("Invalid connection lease"));
    }
    let lease = match open_existing(&path, true) {
      Ok(file) => file,
      Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
      Err(error) => return Err(error),
    };
    match lease.try_lock() {
      Ok(()) => {
        let _ = fs::remove_file(path.with_extension("json"));
        let _ = fs::remove_file(path.with_extension("tmp"));
        drop(lease);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("lease"));
        let _ = fs::remove_file(path.with_extension("locktmp"));
      }
      Err(TryLockError::WouldBlock) => {
        let record_path = path.with_extension("json");
        let mut bytes = Vec::new();
        match open_existing(&record_path, false) {
          Ok(file) => {
            file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
          }
          // Publication and ordinary teardown can race this observation.
          Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
          Err(error) => return Err(error),
        }
        if bytes.len() as u64 > MAX_RECORD_BYTES {
          return Err(io::Error::other("Oversized connection record"));
        }
        let record: ServiceConnection = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if record.connection_id != id
          || record
            .client
            .as_ref()
            .is_some_and(|client| !client.validate())
          || !matches!(record.service.as_str(), "ctmux" | "task" | "vpn")
          || record
            .source
            .as_deref()
            .is_some_and(|source| !valid_label(source, 128))
        {
          return Err(io::Error::other("Invalid connection record"));
        }
        records.push(record);
      }
      Err(TryLockError::Error(error)) => return Err(error),
    }
  }
  records.sort_by(|left, right| {
    left
      .connected_at_ms
      .cmp(&right.connected_at_ms)
      .then_with(|| left.connection_id.cmp(&right.connection_id))
  });
  Ok(records)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn active_channels_are_visible_and_detach_removes_them_without_a_daemon() {
    let root = std::env::temp_dir().join(format!("ctl-presence-{}", uuid::Uuid::new_v4()));
    let client = crate::presence::local_identity("fixture-client", "0.1.2");
    assert_eq!(
      connections_in(&root).unwrap(),
      Vec::<ServiceConnection>::new()
    );
    let first = register_at(&root, Some(client.clone()), "ctmux", None).unwrap();
    let second = register_at(&root, Some(client), "task", None).unwrap();
    let records = connections_in(&root).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].client, records[1].client);
    drop(first);
    assert_eq!(connections_in(&root).unwrap().len(), 1);
    drop(second);
    assert_eq!(
      connections_in(&root).unwrap(),
      Vec::<ServiceConnection>::new()
    );
    fs::remove_dir_all(root).unwrap();
  }

  #[test]
  fn unlocked_crash_records_are_removed_without_pid_or_clock_checks() {
    let root = std::env::temp_dir().join(format!("ctl-presence-{}", uuid::Uuid::new_v4()));
    let mut registration = register_at(&root, None, "ctmux", None).unwrap();
    drop(registration.lease.take());
    assert_eq!(
      connections_in(&root).unwrap(),
      Vec::<ServiceConnection>::new()
    );
    assert!(!registration.record_path.exists());
    drop(registration);
    fs::remove_dir_all(root).unwrap();
  }

  #[cfg(unix)]
  #[test]
  fn inspection_rejects_symlinks_and_nonregular_records_without_following_them() {
    use std::os::unix::fs::symlink;
    let root = std::env::temp_dir().join(format!("ctl-presence-{}", uuid::Uuid::new_v4()));
    let registration = register_at(&root, None, "ctmux", None).unwrap();
    let alias = root.with_extension("alias");
    symlink(&root, &alias).unwrap();
    assert!(connections_in(&alias).is_err());
    fs::remove_file(&alias).unwrap();
    fs::remove_file(&registration.record_path).unwrap();
    assert!(
      std::process::Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(&registration.record_path)
        .status()
        .unwrap()
        .success()
    );
    assert!(connections_in(&root).is_err());
    drop(registration);
    fs::remove_dir_all(root).unwrap();
  }
}
