use rmux_proto::{ArchivedTerminal, SessionArchive, SessionStatus, TerminalEndReason};
use serde::{Serialize, de::DeserializeOwned};
use std::{
  fs, io,
  path::{Path, PathBuf},
  sync::Mutex,
  time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub struct ArchiveStore {
  directory: PathBuf,
  retention_ms: u64,
  gate: Mutex<()>,
}

pub fn now_ms() -> u64 {
  u64::try_from(
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis(),
  )
  .unwrap_or(u64::MAX)
}

impl ArchiveStore {
  pub fn new(directory: PathBuf, retention_days: u64) -> io::Result<Self> {
    private_directory(&directory)?;
    let store = Self {
      directory,
      retention_ms: retention_days.saturating_mul(86_400_000),
      gate: Mutex::new(()),
    };
    // No PTYs survive a daemon restart. In-progress records are now confirmed missing.
    for entry in fs::read_dir(&store.directory)? {
      let entry = entry?;
      if Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err() {
        continue;
      }
      let index = entry.path().join("session.json");
      let Ok(mut archive) = read::<SessionArchive>(&index) else {
        continue;
      };
      if archive.archived_at_ms == 0 {
        archive.archived_at_ms = now_ms();
        archive.expires_at_ms = archive.archived_at_ms.saturating_add(store.retention_ms);
        for terminal in &mut archive.terminals {
          if terminal.ended_at_ms == 0 {
            terminal.ended_at_ms = archive.archived_at_ms;
            terminal.reason = TerminalEndReason::Missing;
          }
        }
        write(&index, &archive)?;
      }
    }
    store.list()?;
    Ok(store)
  }

  pub fn save(
    &self,
    mut archive: SessionArchive,
    terminals: Vec<ArchivedTerminal>,
    complete: bool,
  ) -> io::Result<()> {
    let _guard = self
      .gate
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let directory = self.path(&archive.session.session_id)?;
    private_directory(&directory)?;
    let index = directory.join("session.json");
    if let Ok(previous) = read::<SessionArchive>(&index) {
      // Keep the last full composition when exits merely shrink its membership.
      if archive.view.terminals.len() < previous.view.terminals.len()
        && archive.view.terminals.iter().all(|t| {
          previous
            .view
            .terminals
            .iter()
            .any(|p| p.terminal_id == t.terminal_id)
        })
      {
        archive.view = previous.view;
      }
      archive.terminals = previous.terminals;
    }
    for terminal in terminals {
      let id = &terminal.info.terminal_id;
      if Uuid::parse_str(id).is_err() {
        return Err(io::Error::other("invalid terminal ID"));
      }
      archive.terminals.retain(|info| info.terminal_id != *id);
      archive.terminals.push(terminal.info.clone());
      write(&directory.join(format!("{id}.json")), &terminal)?;
    }
    archive.session.status = SessionStatus::Exited;
    let completed_at_ms = now_ms();
    archive.archived_at_ms = if complete { completed_at_ms } else { 0 };
    archive.expires_at_ms = completed_at_ms.saturating_add(self.retention_ms);
    // Commit the index only after every referenced terminal snapshot is durable.
    write(&index, &archive)
  }

  pub fn list(&self) -> io::Result<Vec<SessionArchive>> {
    let _guard = self
      .gate
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut archives = Vec::new();
    for entry in fs::read_dir(&self.directory)? {
      let entry = entry?;
      if Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err() {
        continue;
      }
      let Ok(archive) = read::<SessionArchive>(&entry.path().join("session.json")) else {
        continue;
      };
      if archive.archived_at_ms == 0 {
        continue;
      }
      if archive.expires_at_ms <= now_ms() {
        fs::remove_dir_all(entry.path())?;
      } else {
        archives.push(archive);
      }
    }
    archives.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
    Ok(archives)
  }

  pub fn get(&self, id: &str) -> io::Result<SessionArchive> {
    let _guard = self
      .gate
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let archive: SessionArchive = read(&self.path(id)?.join("session.json"))?;
    if archive.archived_at_ms == 0 || archive.expires_at_ms <= now_ms() {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "archive is unavailable or expired",
      ));
    }
    Ok(archive)
  }

  pub fn terminal(&self, session_id: &str, terminal_id: &str) -> io::Result<ArchivedTerminal> {
    Uuid::parse_str(terminal_id)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid terminal ID"))?;
    let archive = self.get(session_id)?;
    let info = archive
      .terminals
      .into_iter()
      .find(|info| info.terminal_id == terminal_id)
      .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "archived terminal not found"))?;
    let mut terminal: ArchivedTerminal =
      read(&self.path(session_id)?.join(format!("{terminal_id}.json")))?;
    terminal.info = info;
    Ok(terminal)
  }

  fn path(&self, id: &str) -> io::Result<PathBuf> {
    Uuid::parse_str(id)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid archive ID"))?;
    Ok(self.directory.join(id))
  }
}

fn private_directory(path: &Path) -> io::Result<()> {
  fs::create_dir_all(path)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}

fn read<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
  serde_json::from_reader(fs::File::open(path)?).map_err(io::Error::other)
}

fn write(path: &Path, value: &impl Serialize) -> io::Result<()> {
  use std::io::Write;
  let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
  let result = (|| {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
      use std::os::unix::fs::OpenOptionsExt;
      options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(&temporary, path)
  })();
  if result.is_err() {
    let _ = fs::remove_file(temporary);
  }
  result
}
