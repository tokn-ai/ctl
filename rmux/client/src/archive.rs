//! Client-owned, read-only session records. No daemon connection is involved.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
  fs, io,
  path::PathBuf,
  time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchivedPane {
  pub terminal_id: String,
  pub reason: String,
  pub lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionArchive {
  pub session_id: String,
  pub name: String,
  pub host_key: String,
  pub archived_at_ms: u64,
  pub expires_at_ms: u64,
  pub terminals: Vec<ArchivedPane>,
}

fn now_ms() -> u64 {
  u64::try_from(
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis(),
  )
  .unwrap_or(u64::MAX)
}

pub struct ArchiveStore {
  directory: PathBuf,
}

impl ArchiveStore {
  /// Opens a client-specific local store, without contacting a daemon.
  /// # Errors
  /// Returns an error when no local data directory is available.
  pub fn for_client(client: &str) -> io::Result<Self> {
    let base =
      dirs::data_local_dir().ok_or_else(|| io::Error::other("Local data directory unavailable"))?;
    Ok(Self::new(base.join("rmux").join(client).join("archives")))
  }

  #[must_use]
  pub fn new(directory: PathBuf) -> Self {
    Self { directory }
  }

  /// Atomically retains locally observed output for seven days.
  /// # Errors
  /// Returns filesystem or serialization errors; callers should keep the tab open.
  pub fn save(&self, mut archive: SessionArchive) -> io::Result<()> {
    fs::create_dir_all(&self.directory)?;
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
    }
    archive.archived_at_ms = now_ms();
    archive.expires_at_ms = archive.archived_at_ms.saturating_add(7 * 86_400_000);
    let key = format!(
      "{:x}",
      Sha256::digest(format!("{}\0{}", archive.host_key, archive.session_id))
    );
    let path = self.directory.join(format!("{key}.json"));
    if let Ok(file) = fs::File::open(&path)
      && let Ok(previous) = serde_json::from_reader::<_, SessionArchive>(file)
      && previous.expires_at_ms > now_ms()
    {
      for old in previous.terminals {
        if let Some(pane) = archive
          .terminals
          .iter_mut()
          .find(|pane| pane.terminal_id == old.terminal_id)
        {
          if pane.lines.is_empty() {
            pane.lines = old.lines;
          }
        } else {
          archive.terminals.push(old);
        }
      }
    }
    let temporary = self.directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
      use io::Write;
      let mut options = fs::OpenOptions::new();
      options.write(true).create_new(true);
      #[cfg(unix)]
      {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
      }
      let mut file = options.open(&temporary)?;
      serde_json::to_writer(&mut file, &archive).map_err(io::Error::other)?;
      file.flush()?;
      file.sync_all()?;
      fs::rename(&temporary, path)
    })();
    if result.is_err() {
      let _ = fs::remove_file(temporary);
    }
    result
  }

  /// Lists unexpired records and removes expired files.
  /// # Errors
  /// Returns filesystem or malformed-record errors.
  pub fn list(&self) -> io::Result<Vec<SessionArchive>> {
    let entries = match fs::read_dir(&self.directory) {
      Ok(entries) => entries,
      Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(error) => return Err(error),
    };
    let mut archives = Vec::new();
    for entry in entries {
      let path = entry?.path();
      if path.extension().is_none_or(|extension| extension != "json") {
        continue;
      }
      let archive: SessionArchive =
        serde_json::from_reader(fs::File::open(&path)?).map_err(io::Error::other)?;
      if archive.expires_at_ms <= now_ms() {
        fs::remove_file(path)?;
      } else {
        archives.push(archive);
      }
    }
    archives.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
    Ok(archives)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn local_records_survive_restart_are_host_scoped_and_expire() -> io::Result<()> {
    let directory =
      std::env::temp_dir().join(format!("rmux-client-archives-{}", uuid::Uuid::new_v4()));
    let store = ArchiveStore::new(directory.clone());
    let record = SessionArchive {
      session_id: "missing-session".into(),
      name: "old shell".into(),
      host_key: "offline-host".into(),
      archived_at_ms: 0,
      expires_at_ms: 0,
      terminals: vec![ArchivedPane {
        terminal_id: "pane".into(),
        reason: "Missing".into(),
        lines: vec!["final output".into()],
      }],
    };
    store.save(record.clone())?;
    store.save(SessionArchive {
      host_key: "another-host".into(),
      ..record
    })?;
    let reopened = ArchiveStore::new(directory.clone());
    let archives = reopened.list()?;
    assert_eq!(archives.len(), 2);
    assert_eq!(archives[0].terminals[0].lines, ["final output"]);
    assert_eq!(
      archives[0].expires_at_ms - archives[0].archived_at_ms,
      7 * 86_400_000
    );
    for entry in fs::read_dir(&directory)? {
      let path = entry?.path();
      let mut archive: SessionArchive = serde_json::from_reader(fs::File::open(&path)?)?;
      archive.expires_at_ms = 1;
      fs::write(&path, serde_json::to_vec(&archive)?)?;
    }
    assert!(reopened.list()?.is_empty());
    assert_eq!(fs::read_dir(&directory)?.count(), 0);
    fs::remove_dir_all(directory)
  }
}
