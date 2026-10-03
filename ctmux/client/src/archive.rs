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
  #[serde(default)]
  pub history_gap: bool,
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
  /// Returns an error when the current user's home directory is unavailable.
  pub fn for_client(client: &str) -> io::Result<Self> {
    let base = ctl_core::paths::directory()?;
    Ok(Self::new(base.join("ctmux").join(client).join("archives")))
  }

  #[must_use]
  pub fn new(directory: PathBuf) -> Self {
    Self { directory }
  }

  /// Atomically retains locally observed output until explicitly deleted.
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
    archive.expires_at_ms = 0;
    let key = format!(
      "{:x}",
      Sha256::digest(format!("{}\0{}", archive.host_key, archive.session_id))
    );
    let path = self.directory.join(format!("{key}.json"));
    if let Ok(file) = fs::File::open(&path)
      && let Ok(previous) = serde_json::from_reader::<_, SessionArchive>(file)
    {
      for old in previous.terminals {
        if let Some(pane) = archive
          .terminals
          .iter_mut()
          .find(|pane| pane.terminal_id == old.terminal_id)
        {
          if pane.lines.is_empty() {
            pane.lines = old.lines;
            pane.history_gap = old.history_gap;
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

  /// Deletes a client-owned archive.
  /// # Errors
  /// Returns a filesystem error.
  pub fn delete(&self, host_key: &str, session_id: &str) -> io::Result<()> {
    let key = format!("{:x}", Sha256::digest(format!("{host_key}\0{session_id}")));
    match fs::remove_file(self.directory.join(format!("{key}.json"))) {
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
      result => result,
    }
  }

  /// Lists records, including legacy records with a former expiry timestamp.
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
      archives.push(archive);
    }
    archives.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
    Ok(archives)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn legacy_panes_have_no_synthetic_gap_and_new_gaps_round_trip() {
    let mut pane: ArchivedPane =
      serde_json::from_str(r#"{"terminal_id":"old","reason":"ended","lines":["kept"]}"#).unwrap();
    assert!(!pane.history_gap);
    pane.history_gap = true;
    let decoded: ArchivedPane =
      serde_json::from_value(serde_json::to_value(&pane).unwrap()).unwrap();
    assert!(decoded.history_gap);
    assert_eq!(decoded.lines, ["kept"]);
  }

  #[test]
  fn local_records_survive_restart_are_host_scoped_and_require_deletion() -> io::Result<()> {
    let directory =
      std::env::temp_dir().join(format!("ctmux-client-archives-{}", uuid::Uuid::new_v4()));
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
        history_gap: false,
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
    assert_eq!(archives[0].expires_at_ms, 0);
    for entry in fs::read_dir(&directory)? {
      let path = entry?.path();
      let mut archive: SessionArchive = serde_json::from_reader(fs::File::open(&path)?)?;
      archive.expires_at_ms = 1;
      fs::write(&path, serde_json::to_vec(&archive)?)?;
    }
    assert_eq!(reopened.list()?.len(), 2);
    reopened.delete("offline-host", "missing-session")?;
    assert_eq!(reopened.list()?.len(), 1);
    reopened.delete("another-host", "missing-session")?;
    assert_eq!(
      reopened
        .list()?
        .into_iter()
        .map(|archive| (archive.host_key, archive.session_id))
        .collect::<Vec<_>>(),
      Vec::<(String, String)>::new()
    );
    fs::remove_dir_all(directory)
  }
}
