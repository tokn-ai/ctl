//! Durable client-side scrollback and the latest terminal presentation.
//! Raw redraw frames are never retained. History commits precede the atomic
//! current snapshot; its byte offset lets recovery discard an interrupted tail.
use crate::{
  AttachmentEvent,
  archive::{ArchivedPane, SessionArchive},
};
use ctmux_proto::{TerminalCheckpoint, TerminalSize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
  fs::{self, File, OpenOptions},
  io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
  path::{Path, PathBuf},
  time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheIdentity {
  pub host_key: String,
  pub session_id: String,
  pub terminal_id: String,
  pub name: String,
  pub terminal_size: TerminalSize,
  pub primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Current {
  version: u16,
  identity: CacheIdentity,
  sequence: u64,
  history_bytes: u64,
  wrapped_prefix: String,
  pending_utf8: Vec<u8>,
  payload: String,
  screen_lines: Vec<String>,
  history_gap: bool,
  reason: Option<String>,
  archived_at_ms: u64,
}

pub struct CachedPresentation {
  pub terminal_id: String,
  pub checkpoint: TerminalCheckpoint,
  pub history: Vec<String>,
  pub history_gap: bool,
}

pub struct ArchivePage {
  pub lines: Vec<String>,
  pub next_offset: Option<u64>,
}

pub struct CacheStore {
  directory: PathBuf,
}

fn invalid(message: &str) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, message)
}
fn key(value: &str) -> String {
  format!("{:x}", Sha256::digest(value))
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
fn private_directory(path: &Path) -> io::Result<()> {
  fs::create_dir_all(path)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}
fn private_file(path: &Path, append: bool) -> io::Result<File> {
  let mut options = OpenOptions::new();
  options
    .read(true)
    .write(true)
    .create(true)
    .truncate(false)
    .append(append);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  options.open(path)
}
fn sync_directory(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  File::open(path)?.sync_all()?;
  #[cfg(not(unix))]
  let _ignored = path;
  Ok(())
}
fn atomic_json(path: &Path, value: &Current) -> io::Result<()> {
  let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = private_file(&temporary, false)?;
    serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_directory(
      path
        .parent()
        .ok_or_else(|| invalid("Missing snapshot parent"))?,
    )?;
    Ok(())
  })();
  if result.is_err() {
    let _ignored = fs::remove_file(temporary);
  }
  result
}
fn read_current(directory: &Path) -> io::Result<Option<Current>> {
  match File::open(directory.join("current.json")) {
    Ok(file) => {
      let current: Current = serde_json::from_reader(file).map_err(io::Error::other)?;
      if current.version != 1 {
        return Err(invalid("Unsupported local session cache version"));
      }
      if current.identity.terminal_size.columns == 0 || current.identity.terminal_size.rows == 0 {
        return Err(invalid("Invalid cached terminal dimensions"));
      }
      Ok(Some(current))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}
fn history(directory: &Path, current: &Current, limit: Option<usize>) -> io::Result<Vec<String>> {
  let file = match File::open(directory.join("history.jsonl")) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound && current.history_bytes == 0 => {
      return Ok(Vec::new());
    }
    Err(error) => return Err(error),
  };
  if file.metadata()?.len() < current.history_bytes {
    return Err(invalid("Local session history is incomplete"));
  }
  let mut result = std::collections::VecDeque::new();
  for line in BufReader::new(file.take(current.history_bytes)).lines() {
    result.push_back(serde_json::from_str(&line?).map_err(io::Error::other)?);
    if limit.is_some_and(|limit| result.len() > limit) {
      result.pop_front();
    }
  }
  Ok(result.into())
}
fn archive_screen_lines(current: &Current) -> Vec<String> {
  let size = &current.identity.terminal_size;
  let mut terminal =
    avt::terminal::Terminal::new((usize::from(size.columns), usize::from(size.rows)), Some(0));
  let mut parser = avt::parser::Parser::default();
  for character in current.payload.chars() {
    if let Some(function) = parser.feed(character) {
      terminal.execute(function);
    }
  }

  let mut screen = Vec::new();
  let mut prefix = current.wrapped_prefix.clone();
  if terminal.active_buffer_type() == avt::terminal::BufferType::Alternate && !prefix.is_empty() {
    // A partially scrolled primary line cannot continue in the alternate screen.
    screen.push(std::mem::take(&mut prefix));
  }
  let mut unwrapper = avt::util::TextUnwrapper::new();
  let last_content_row = terminal
    .view()
    .enumerate()
    .filter_map(|(row, line)| {
      let wrapped = unwrapper.push(line).is_none();
      (wrapped || !line.text().trim_end().is_empty()).then_some(row)
    })
    .last();
  let cursor = terminal.cursor();
  if last_content_row.is_none() && cursor == (0, 0) && prefix.is_empty() {
    return screen;
  }
  // Keep cursor-reached blank lines while excluding unused rows below the screen's content.
  let last_row = cursor.row.max(last_content_row.unwrap_or(0));
  let mut unwrapper = avt::util::TextUnwrapper::new();
  for line in terminal.view().take(last_row + 1) {
    if let Some(text) = unwrapper.push(line) {
      prefix.push_str(&text);
      screen.push(std::mem::take(&mut prefix));
    }
  }
  if let Some(text) = unwrapper.flush() {
    prefix.push_str(&text);
  }
  if !prefix.is_empty() {
    screen.push(prefix);
  }
  screen
}

fn emulator(size: &TerminalSize) -> avt::Vt {
  avt::Vt::builder()
    .size(usize::from(size.columns), usize::from(size.rows))
    .scrollback_limit(0)
    .build()
}
fn collect(lines: impl Iterator<Item = avt::Line>, prefix: &mut String) -> Vec<String> {
  let mut result = Vec::new();
  let mut unwrapper = avt::util::TextUnwrapper::new();
  for line in lines {
    if let Some(text) = unwrapper.push(&line) {
      prefix.push_str(&text);
      result.push(std::mem::take(prefix));
    }
  }
  if let Some(text) = unwrapper.flush() {
    prefix.push_str(&text);
  }
  result
}

fn update_current(
  current: &mut Current,
  vt: &mut avt::Vt,
  directory: &Path,
  event: &AttachmentEvent,
  has_previous: bool,
) -> io::Result<Option<Vec<String>>> {
  let mut appended = Vec::new();
  match event {
    AttachmentEvent::Checkpoint {
      checkpoint,
      history: snapshot,
      history_gap,
    } => {
      if !checkpoint.is_supported()
        || !snapshot.is_supported()
        || checkpoint.sequence != snapshot.sequence
      {
        return Err(invalid("Unsupported local checkpoint"));
      }
      if has_previous && checkpoint.sequence < current.sequence {
        return Ok(None);
      }
      if !has_previous || checkpoint.sequence > current.sequence {
        let retained = history(directory, current, Some(snapshot.lines.len()))?;
        let overlap = (0..=retained.len().min(snapshot.lines.len()))
          .rev()
          .find(|&length| retained[retained.len() - length..] == snapshot.lines[..length])
          .unwrap_or(0);
        appended.extend_from_slice(&snapshot.lines[overlap..]);
      }
      current.identity.terminal_size = checkpoint.terminal_size.clone();
      current.pending_utf8.clone_from(&checkpoint.input_prefix);
      if !has_previous || checkpoint.sequence != current.sequence {
        current.wrapped_prefix.clear();
      }
      current.history_gap |= *history_gap || snapshot.truncated;
      current.sequence = checkpoint.sequence;
      *vt = emulator(&checkpoint.terminal_size);
      drop(vt.feed_str(std::str::from_utf8(&checkpoint.payload).map_err(io::Error::other)?));
    }
    AttachmentEvent::Output {
      sequence_start,
      sequence_end,
      data,
    } => {
      if sequence_end < sequence_start {
        return Err(invalid("Invalid local output range"));
      }
      if has_previous && *sequence_end <= current.sequence {
        return Ok(None);
      }
      appended = update_output(current, vt, *sequence_start, *sequence_end, data)?;
    }
    AttachmentEvent::PtyGeometryChanged {
      terminal_size,
      observed_sequence,
    } => {
      if *observed_sequence < current.sequence {
        return Ok(None);
      }
      appended.extend(collect(
        vt.resize(
          usize::from(terminal_size.columns),
          usize::from(terminal_size.rows),
        )
        .scrollback,
        &mut current.wrapped_prefix,
      ));
      current.identity.terminal_size = terminal_size.clone();
    }
    _ => unreachable!(),
  }
  Ok(Some(appended))
}

fn update_output(
  current: &mut Current,
  vt: &mut avt::Vt,
  start: u64,
  end: u64,
  data: &[u8],
) -> io::Result<Vec<String>> {
  let mut appended = Vec::new();
  if start > current.sequence {
    return Err(invalid(
      "Local session output has a gap; reconnect to restore a checkpoint",
    ));
  }
  let offset = usize::try_from(current.sequence - start).map_err(io::Error::other)?;
  if end - start != u64::try_from(data.len()).map_err(io::Error::other)? || offset > data.len() {
    return Err(invalid("Invalid local output range"));
  }
  current.pending_utf8.extend_from_slice(&data[offset..]);
  loop {
    let (text, consumed) = match std::str::from_utf8(&current.pending_utf8) {
      Ok(valid) => (valid.to_owned(), valid.len()),
      Err(error) if error.valid_up_to() > 0 => {
        let length = error.valid_up_to();
        (
          String::from_utf8(current.pending_utf8[..length].to_vec()).map_err(io::Error::other)?,
          length,
        )
      }
      Err(error) => match error.error_len() {
        Some(length) => ("\u{fffd}".into(), length),
        None => break,
      },
    };
    if consumed == 0 {
      break;
    }
    appended.extend(collect(
      vt.feed_str(&text).scrollback,
      &mut current.wrapped_prefix,
    ));
    current.pending_utf8.drain(..consumed);
  }
  current.sequence = end;
  Ok(appended)
}

fn copy_record(source: &Path, destination: &Path) -> io::Result<()> {
  private_directory(destination)?;
  for entry in fs::read_dir(source)? {
    let entry = entry?;
    let target = destination.join(entry.file_name());
    if entry.file_type()?.is_dir() {
      copy_record(&entry.path(), &target)?;
    } else if entry.file_name() == "current.json" || entry.file_name() == "history.jsonl" {
      fs::copy(entry.path(), &target)?;
      File::open(target)?.sync_all()?;
    }
  }
  sync_directory(destination)
}
fn resume_archive(archived: &Path, live: &Path) -> io::Result<()> {
  let parent = live
    .parent()
    .ok_or_else(|| invalid("Missing live cache parent"))?;
  private_directory(parent)?;
  let temporary = live.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
  let result = copy_record(archived, &temporary).and_then(|()| {
    fs::rename(&temporary, live)?;
    sync_directory(parent)
  });
  if result.is_err() {
    let _ignored = fs::remove_dir_all(temporary);
  }
  result
}
fn recover_archive(committed: &Path) -> io::Result<()> {
  let previous = committed.with_extension("previous");
  if previous.exists() {
    if committed.exists() {
      fs::remove_dir_all(previous)?;
    } else {
      fs::rename(previous, committed)?;
    }
  }
  Ok(())
}
fn recover_archives(directory: &Path) -> io::Result<()> {
  if !directory.exists() {
    return Ok(());
  }
  for entry in fs::read_dir(directory)? {
    let path = entry?.path();
    if path
      .extension()
      .is_some_and(|extension| extension == "previous")
    {
      let committed = path.with_extension("");
      if committed.exists() {
        fs::remove_dir_all(path)?;
      } else {
        fs::rename(path, committed)?;
      }
    }
  }
  Ok(())
}

fn select_primary(root: &Path, selected: &Path) -> io::Result<()> {
  for entry in fs::read_dir(root)? {
    let directory = entry?.path();
    if directory == selected {
      continue;
    }
    if let Some(mut current) = read_current(&directory)?
      && current.identity.primary
    {
      current.identity.primary = false;
      atomic_json(&directory.join("current.json"), &current)?;
    }
  }
  Ok(())
}

impl CacheStore {
  /// # Errors
  /// Returns an error when the current user's home directory is unavailable.
  pub fn for_client(client: &str) -> io::Result<Self> {
    let base = ctl_core::paths::directory()?;
    Ok(Self::new(base.join("ctmux").join(client).join("sessions")))
  }
  #[must_use]
  pub fn new(directory: PathBuf) -> Self {
    Self { directory }
  }
  fn lock(&self) -> io::Result<File> {
    private_directory(&self.directory)?;
    let file = private_file(&self.directory.join("store.lock"), false)?;
    file.lock()?;
    Ok(file)
  }
  fn session_path(&self, host_key: &str, session_id: &str, archived: bool) -> PathBuf {
    self
      .directory
      .join(if archived { "archives" } else { "live" })
      .join(key(&format!("{host_key}\0{session_id}")))
  }
  /// Saves completed scrollback and replaces the latest screen, before event acknowledgement.
  /// # Errors
  /// Returns storage errors or an invalid/gapped output sequence.
  pub fn apply(&self, identity: &CacheIdentity, event: &AttachmentEvent) -> io::Result<()> {
    if !matches!(
      event,
      AttachmentEvent::Checkpoint { .. }
        | AttachmentEvent::Output { .. }
        | AttachmentEvent::PtyGeometryChanged { .. }
    ) {
      return Ok(());
    }
    let _lock = self.lock()?;
    let root = self.session_path(&identity.host_key, &identity.session_id, false);
    let archived = self.session_path(&identity.host_key, &identity.session_id, true);
    recover_archive(&archived)?;
    if !root.exists() && archived.exists() {
      resume_archive(&archived, &root)?;
    }
    let directory = root.join(key(&identity.terminal_id));
    private_directory(&directory)?;
    let previous = read_current(&directory)?;
    let mut current = previous.clone().unwrap_or_else(|| Current {
      version: 1,
      identity: identity.clone(),
      sequence: 0,
      history_bytes: 0,
      wrapped_prefix: String::new(),
      pending_utf8: Vec::new(),
      payload: String::new(),
      screen_lines: Vec::new(),
      history_gap: false,
      reason: None,
      archived_at_ms: 0,
    });
    let mut vt = emulator(&current.identity.terminal_size);
    drop(vt.feed_str(&current.payload));
    let Some(appended) =
      update_current(&mut current, &mut vt, &directory, event, previous.is_some())?
    else {
      return Ok(());
    };
    current.identity.name.clone_from(&identity.name);
    if identity.primary
      && previous
        .as_ref()
        .is_none_or(|current| !current.identity.primary)
    {
      select_primary(&root, &directory)?;
    }
    current.identity.primary |= identity.primary;
    current.payload = vt.dump();
    current.screen_lines = vt
      .view()
      .map(|line| line.text().trim_end().to_owned())
      .collect();
    current.reason = None;
    current.archived_at_ms = 0;
    let mut file = private_file(&directory.join("history.jsonl"), true)?;
    if file.metadata()?.len() < current.history_bytes {
      return Err(invalid("Local session history is incomplete"));
    }
    file.set_len(current.history_bytes)?;
    for line in appended {
      let bytes = serde_json::to_vec(&line).map_err(io::Error::other)?;
      file.write_all(&bytes)?;
      file.write_all(b"\n")?;
      current.history_bytes += u64::try_from(bytes.len() + 1).map_err(io::Error::other)?;
    }
    file.sync_all()?;
    atomic_json(&directory.join("current.json"), &current)
  }
  /// # Errors
  /// Returns storage or corrupt-record errors.
  pub fn load(
    &self,
    host_key: &str,
    session_id: &str,
    terminal_id: Option<&str>,
  ) -> io::Result<Option<CachedPresentation>> {
    let _lock = self.lock()?;
    recover_archive(&self.session_path(host_key, session_id, true))?;
    for archived in [false, true] {
      let root = self.session_path(host_key, session_id, archived);
      let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
        Err(error) => return Err(error),
      };
      let mut candidates = Vec::new();
      for entry in entries {
        let directory = entry?.path();
        if let Some(current) = read_current(&directory)?
          && terminal_id.is_none_or(|id| current.identity.terminal_id == id)
        {
          candidates.push((directory, current));
        }
      }
      candidates.sort_by_key(|(_, current)| {
        (
          !current.identity.primary,
          current.identity.terminal_id.clone(),
        )
      });
      if let Some((directory, current)) = candidates.into_iter().next() {
        let mut lines = history(&directory, &current, Some(10_000))?;
        // The durable transcript is unbounded; the renderer receives a bounded tail.
        if lines.len() > 10_000 {
          lines.drain(..lines.len() - 10_000);
        }
        if !current.wrapped_prefix.is_empty() {
          lines.push(current.wrapped_prefix.clone());
        }
        return Ok(Some(CachedPresentation {
          terminal_id: current.identity.terminal_id,
          checkpoint: TerminalCheckpoint {
            format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
            format_version: 1,
            sequence: current.sequence,
            terminal_size: current.identity.terminal_size,
            payload: current.payload.into_bytes(),
            input_prefix: current.pending_utf8,
          },
          history: lines,
          history_gap: current.history_gap,
        }));
      }
    }
    Ok(None)
  }
  /// Seals all panes when the client's tab closes; a later open resumes this cache.
  /// # Errors
  /// Returns storage or corrupt-record errors, retaining the live cache on failure.
  pub fn archive(&self, host_key: &str, session_id: &str, reason: &str) -> io::Result<()> {
    let _lock = self.lock()?;
    recover_archive(&self.session_path(host_key, session_id, true))?;
    let root = self.session_path(host_key, session_id, false);
    if !root.exists() {
      return Ok(());
    }
    for entry in fs::read_dir(&root)? {
      let directory = entry?.path();
      if let Some(mut current) = read_current(&directory)? {
        current.reason = Some(reason.into());
        current.archived_at_ms = now_ms();
        atomic_json(&directory.join("current.json"), &current)?;
      }
    }
    let archived = self.session_path(host_key, session_id, true);
    private_directory(
      archived
        .parent()
        .ok_or_else(|| invalid("Missing archive parent"))?,
    )?;
    let previous = archived.with_extension("previous");
    if archived.exists() {
      fs::rename(&archived, &previous)?;
    }
    if let Err(error) = fs::rename(&root, &archived) {
      if previous.exists() {
        fs::rename(&previous, &archived)?;
      }
      return Err(error);
    }
    sync_directory(
      archived
        .parent()
        .ok_or_else(|| invalid("Missing archive parent"))?,
    )?;
    sync_directory(
      root
        .parent()
        .ok_or_else(|| invalid("Missing live parent"))?,
    )?;
    if previous.exists() {
      fs::remove_dir_all(previous)?;
    }
    Ok(())
  }
  /// # Errors
  /// Returns storage or corrupt-record errors.
  pub fn archives(&self) -> io::Result<Vec<SessionArchive>> {
    let _lock = self.lock()?;
    let root = self.directory.join("archives");
    recover_archives(&root)?;
    if !root.exists() {
      return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
      let mut archive: Option<SessionArchive> = None;
      for pane in fs::read_dir(entry?.path())? {
        let directory = pane?.path();
        let Some(current) = read_current(&directory)? else {
          continue;
        };
        let record = archive.get_or_insert_with(|| SessionArchive {
          session_id: current.identity.session_id.clone(),
          host_key: current.identity.host_key.clone(),
          name: current.identity.name.clone(),
          archived_at_ms: current.archived_at_ms,
          expires_at_ms: 0,
          terminals: Vec::new(),
        });
        record.terminals.push(ArchivedPane {
          terminal_id: current.identity.terminal_id,
          reason: current.reason.unwrap_or_else(|| "Tab closed".into()),
          lines: Vec::new(),
        });
      }
      if let Some(archive) = archive {
        result.push(archive);
      }
    }
    result.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
    Ok(result)
  }
  /// Reads a bounded page without loading the entire transcript into memory.
  /// # Errors
  /// Returns storage, corrupt-record, or invalid-offset errors.
  pub fn read_archive(
    &self,
    host_key: &str,
    session_id: &str,
    terminal_id: &str,
    offset: u64,
  ) -> io::Result<Option<ArchivePage>> {
    let _lock = self.lock()?;
    recover_archive(&self.session_path(host_key, session_id, true))?;
    let directory = self
      .session_path(host_key, session_id, true)
      .join(key(terminal_id));
    let Some(current) = read_current(&directory)? else {
      return Ok(None);
    };
    if offset > current.history_bytes {
      return Err(invalid("Invalid archive offset"));
    }
    let mut position = offset;
    let mut lines = Vec::new();
    if offset < current.history_bytes {
      let mut file = File::open(directory.join("history.jsonl"))?;
      if file.metadata()?.len() < current.history_bytes {
        return Err(invalid("Local session history is incomplete"));
      }
      file.seek(SeekFrom::Start(offset))?;
      let mut reader = BufReader::new(file.take(current.history_bytes - offset));
      while lines.len() < 1000 {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
          break;
        }
        position += u64::try_from(bytes).map_err(io::Error::other)?;
        lines.push(serde_json::from_str(&line).map_err(io::Error::other)?);
      }
    }
    let next_offset = (position < current.history_bytes).then_some(position);
    if next_offset.is_none() {
      lines.extend(archive_screen_lines(&current));
    }
    Ok(Some(ArchivePage { lines, next_offset }))
  }

  /// # Errors
  /// Returns a filesystem error.
  pub fn delete_archive(&self, host_key: &str, session_id: &str) -> io::Result<()> {
    let _lock = self.lock()?;
    let path = self.session_path(host_key, session_id, true);
    recover_archive(&path)?;
    if path.exists() {
      fs::remove_dir_all(path)?;
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  struct Fixture {
    store: CacheStore,
    identity: CacheIdentity,
  }
  impl Fixture {
    fn new() -> Self {
      let directory =
        std::env::temp_dir().join(format!("ctmux-cache-test-{}", uuid::Uuid::new_v4()));
      Self {
        store: CacheStore::new(directory),
        identity: CacheIdentity {
          host_key: "local".into(),
          session_id: "session".into(),
          terminal_id: "pane".into(),
          name: "shell".into(),
          terminal_size: TerminalSize {
            columns: 20,
            rows: 2,
            ..TerminalSize::default()
          },
          primary: true,
        },
      }
    }
    fn output(&self, start: u64, text: &str) -> io::Result<u64> {
      let end = start + u64::try_from(text.len()).map_err(io::Error::other)?;
      self.store.apply(
        &self.identity,
        &AttachmentEvent::Output {
          sequence_start: start,
          sequence_end: end,
          data: text.as_bytes().to_vec(),
        },
      )?;
      Ok(end)
    }
    fn checkpoint(&self, sequence: u64, text: &str, lines: &[&str]) -> io::Result<()> {
      self.store.apply(
        &self.identity,
        &AttachmentEvent::Checkpoint {
          checkpoint: TerminalCheckpoint {
            format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
            format_version: 1,
            sequence,
            terminal_size: self.identity.terminal_size.clone(),
            payload: text.as_bytes().to_vec(),
            input_prefix: Vec::new(),
          },
          history: ctmux_proto::TerminalHistorySnapshot {
            format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
            format_version: 1,
            sequence,
            generation: 0,
            revision: 0,
            retained_bytes: 0,
            truncated: false,
            lines: lines.iter().map(|line| (*line).into()).collect(),
          },
          history_gap: false,
        },
      )
    }
    fn presentation(&self) -> io::Result<CachedPresentation> {
      self
        .store
        .load("local", "session", Some("pane"))?
        .ok_or_else(|| invalid("Missing test presentation"))
    }
    fn archive(&self) -> io::Result<ArchivePage> {
      self.store.archive("local", "session", "Closed")?;
      self
        .store
        .read_archive("local", "session", "pane", 0)?
        .ok_or_else(|| invalid("Missing archive page"))
    }
  }
  impl Drop for Fixture {
    fn drop(&mut self) {
      let _ignored = fs::remove_dir_all(&self.store.directory);
    }
  }

  #[test]
  fn history_survives_restart_and_duplicate_replay() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "", &["older"])?;
    let end = fixture.output(0, "one\r\ntwo\r\nthree")?;
    fixture.output(0, "one\r\ntwo\r\nthree")?;
    let reopened = CacheStore::new(fixture.store.directory.clone());
    let saved = reopened
      .load("local", "session", Some("pane"))?
      .ok_or_else(|| invalid("Missing cache"))?;
    assert_eq!(saved.history, ["older", "one"]);
    assert_eq!(saved.checkpoint.sequence, end);
    let mut vt = emulator(&saved.checkpoint.terminal_size);
    drop(vt.feed_str(std::str::from_utf8(&saved.checkpoint.payload).map_err(io::Error::other)?));
    assert_eq!(
      vt.view()
        .map(|line| line.text().trim_end().to_owned())
        .collect::<Vec<_>>(),
      ["two", "three"]
    );
    Ok(())
  }

  #[test]
  fn tui_repaints_replace_current_without_entering_history() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "shell", &["before"])?;
    let mut sequence = fixture.output(0, "\x1b[?1049h\x1b[2J\x1b[Hframe zero")?;
    for index in 1..100 {
      sequence = fixture.output(sequence, &format!("\x1b[H\x1b[2Jframe {index}"))?;
    }
    let saved = fixture.presentation()?;
    assert_eq!(saved.history, ["before"]);
    let path = fixture
      .store
      .session_path("local", "session", false)
      .join(key("pane"));
    let current = read_current(&path)?.ok_or_else(|| invalid("Missing current"))?;
    assert_eq!(current.screen_lines[0], "frame 99");
    assert!(!current.payload.contains("frame 98"));
    assert_eq!(fs::read_dir(path)?.count(), 2);
    fixture.output(sequence, "\x1b[?1049l")?;
    assert_eq!(fixture.presentation()?.history, ["before"]);
    Ok(())
  }

  #[test]
  fn archival_seals_all_panes_and_reopening_resumes_the_same_history() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "first", &["past"])?;
    let second = CacheIdentity {
      terminal_id: "second".into(),
      primary: false,
      ..fixture.identity.clone()
    };
    fixture.store.apply(
      &second,
      &AttachmentEvent::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"two".to_vec(),
      },
    )?;
    fixture.store.archive("local", "session", "Closed")?;
    let records = fixture.store.archives()?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].terminals.len(), 2);
    assert_eq!(records[0].expires_at_ms, 0);
    assert_eq!(fixture.presentation()?.history, ["past"]);
    fixture.output(0, " next")?;
    assert_eq!(fixture.store.archives()?.len(), 1);
    let frozen = fixture
      .store
      .read_archive("local", "session", "pane", 0)?
      .ok_or_else(|| invalid("Missing frozen archive"))?;
    assert_eq!(frozen.lines[1], "first");
    assert_eq!(fixture.presentation()?.history, ["past"]);
    fixture.store.archive("local", "session", "Closed again")?;
    fixture.store.delete_archive("local", "session")?;
    assert_eq!(
      fixture
        .store
        .archives()?
        .into_iter()
        .map(|archive| (archive.host_key, archive.session_id))
        .collect::<Vec<_>>(),
      Vec::<(String, String)>::new()
    );
    Ok(())
  }

  #[test]
  fn interrupted_history_tail_is_rolled_back_before_the_next_commit() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "", &["committed"])?;
    let path = fixture
      .store
      .session_path("local", "session", false)
      .join(key("pane"))
      .join("history.jsonl");
    private_file(&path, true)?.write_all(b"\"uncommitted\"\n")?;
    assert_eq!(fixture.presentation()?.history, ["committed"]);
    fixture.output(0, "one\r\ntwo\r\nthree")?;
    assert_eq!(fixture.presentation()?.history, ["committed", "one"]);
    Ok(())
  }

  #[test]
  fn split_utf8_wrapped_lines_and_output_gaps_are_handled() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    fixture.checkpoint(0, "", &[])?;
    fixture.store.apply(
      &fixture.identity,
      &AttachmentEvent::Output {
        sequence_start: 0,
        sequence_end: 1,
        data: vec![0xc3],
      },
    )?;
    let reopened = CacheStore::new(fixture.store.directory.clone());
    reopened.apply(
      &fixture.identity,
      &AttachmentEvent::Output {
        sequence_start: 1,
        sequence_end: 2,
        data: vec![0xa9],
      },
    )?;
    fixture.output(2, "abcdef\r\nnext\r\nlast")?;
    assert_eq!(fixture.presentation()?.history[0], "éabcdef");
    let gap = fixture.store.apply(
      &fixture.identity,
      &AttachmentEvent::Output {
        sequence_start: 1000,
        sequence_end: 1001,
        data: vec![b'x'],
      },
    );
    assert!(gap.is_err());
    Ok(())
  }

  #[test]
  fn hosts_are_isolated_and_corrupt_current_is_not_overwritten() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "", &["private"])?;
    assert!(fixture.store.load("other", "session", None)?.is_none());
    let path = fixture
      .store
      .session_path("local", "session", false)
      .join(key("pane"))
      .join("current.json");
    fs::write(&path, b"invalid")?;
    assert!(fixture.output(0, "hello").is_err());
    assert_eq!(fs::read(path)?, b"invalid");
    Ok(())
  }

  #[test]
  fn archive_empty_screen_retains_the_record_without_grid_padding() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "", &[])?;
    let payload = fixture.presentation()?.checkpoint.payload;
    let page = fixture.archive()?;
    assert_eq!(page.lines, Vec::<String>::new());
    assert!(page.next_offset.is_none());
    let archives = fixture.store.archives()?;
    assert_eq!(archives.len(), 1);
    assert_eq!(archives[0].terminals[0].terminal_id, "pane");
    assert_eq!(archives[0].terminals[0].reason, "Closed");
    assert_eq!(fixture.presentation()?.checkpoint.payload, payload);
    Ok(())
  }

  #[test]
  fn archive_short_tail_preserves_history_blanks_and_live_presentation() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.rows = 6;
    fixture.checkpoint(0, "tail", &["before", "", "after", ""])?;
    let saved = fixture.presentation()?;
    assert_eq!(
      fixture.archive()?.lines,
      ["before", "", "after", "", "tail"]
    );
    let restored = fixture.presentation()?;
    assert_eq!(restored.history, saved.history);
    assert_eq!(restored.checkpoint.payload, saved.checkpoint.payload);
    assert_eq!(
      restored.checkpoint.terminal_size,
      saved.checkpoint.terminal_size
    );
    Ok(())
  }

  #[test]
  fn archive_tail_preserves_blank_lines_reached_by_output() -> io::Result<()> {
    for (output, expected) in [
      ("\r\n", vec!["", ""]),
      ("tail\r\n\r\n", vec!["tail", "", ""]),
      ("head\r\n\r\ntail\r\n", vec!["head", "", "tail", ""]),
    ] {
      let mut fixture = Fixture::new();
      fixture.identity.terminal_size.rows = 6;
      fixture.checkpoint(0, "", &[])?;
      fixture.output(0, output)?;
      assert_eq!(fixture.archive()?.lines, expected);
    }
    Ok(())
  }

  #[test]
  fn archive_tail_retains_content_below_a_repositioned_cursor() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.rows = 6;
    fixture.checkpoint(0, "head\r\n\r\ntail\x1b[H", &[])?;
    assert_eq!(fixture.archive()?.lines, ["head", "", "tail"]);
    Ok(())
  }

  #[test]
  fn archive_tail_joins_soft_wrapping_and_retains_interior_blank_lines() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    fixture.identity.terminal_size.rows = 6;
    fixture.checkpoint(0, "abcdef\r\n\r\nhi", &[])?;
    assert_eq!(fixture.archive()?.lines, ["abcdef", "", "hi"]);
    Ok(())
  }

  #[test]
  fn archive_tail_joins_a_partially_scrolled_logical_line() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    fixture.checkpoint(0, "", &[])?;
    fixture.output(0, "abcdefghi")?;
    assert_eq!(fixture.archive()?.lines, ["abcdefghi"]);
    Ok(())
  }

  #[test]
  fn archive_tail_keeps_primary_prefix_separate_from_alternate_screen() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    fixture.checkpoint(0, "", &[])?;
    let sequence = fixture.output(0, "abcdefghi")?;
    fixture.output(sequence, "\x1b[?1049h\x1b[HUI")?;
    assert_eq!(fixture.archive()?.lines, ["abc", "UI"]);
    Ok(())
  }

  #[test]
  fn archive_pages_retain_all_history_and_append_current_only_at_the_end() -> io::Result<()> {
    let fixture = Fixture::new();
    let lines: Vec<String> = (0..2500).map(|index| format!("line {index}")).collect();
    let borrowed: Vec<&str> = lines.iter().map(String::as_str).collect();
    fixture.checkpoint(0, "last screen", &borrowed)?;
    fixture.store.archive("local", "session", "Closed")?;
    assert_eq!(
      fixture.store.archives()?[0].terminals[0].lines,
      Vec::<String>::new()
    );
    let mut offset = 0;
    let mut all = Vec::new();
    loop {
      let page = fixture
        .store
        .read_archive("local", "session", "pane", offset)?
        .ok_or_else(|| invalid("Missing archive page"))?;
      all.extend(page.lines);
      if let Some(next) = page.next_offset {
        assert!(next > offset);
        offset = next;
      } else {
        break;
      }
    }
    assert_eq!(&all[..2500], lines);
    assert_eq!(all[2500], "last screen");
    assert_eq!(all.len(), 2501);
    Ok(())
  }

  #[test]
  fn checkpoint_at_the_saved_sequence_preserves_a_partially_wrapped_history_line() -> io::Result<()>
  {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    fixture.checkpoint(0, "", &[])?;
    let sequence = fixture.output(0, "abcdefghi")?;
    let saved = fixture.presentation()?;
    fixture.checkpoint(
      sequence,
      std::str::from_utf8(&saved.checkpoint.payload).map_err(io::Error::other)?,
      &[],
    )?;
    fixture.output(sequence, "\r\nnext\r\nlast")?;
    assert_eq!(fixture.presentation()?.history[0], "abcdefghi");
    Ok(())
  }

  #[test]
  fn root_preview_tracks_the_current_primary_pane_across_reconnects() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "old root", &[])?;
    let replacement = CacheIdentity {
      terminal_id: "new-root".into(),
      ..fixture.identity.clone()
    };
    fixture.store.apply(
      &replacement,
      &AttachmentEvent::Output {
        sequence_start: 0,
        sequence_end: 3,
        data: b"new".to_vec(),
      },
    )?;
    let reconnected = CacheIdentity {
      primary: false,
      ..replacement.clone()
    };
    fixture.store.apply(
      &reconnected,
      &AttachmentEvent::Output {
        sequence_start: 3,
        sequence_end: 4,
        data: b"!".to_vec(),
      },
    )?;
    let saved = fixture
      .store
      .load("local", "session", None)?
      .ok_or_else(|| invalid("Missing primary"))?;
    assert_eq!(saved.terminal_id, replacement.terminal_id);
    Ok(())
  }

  #[test]
  fn interrupted_archive_replacement_restores_the_previous_snapshot() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "saved", &[])?;
    fixture.store.archive("local", "session", "Closed")?;
    let path = fixture.store.session_path("local", "session", true);
    fs::rename(&path, path.with_extension("previous"))?;
    assert_eq!(fixture.store.archives()?.len(), 1);
    assert!(path.exists());
    Ok(())
  }
}
