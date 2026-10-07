//! Bounded authoritative history, provisional recent output, and the latest screen.
//! New snapshots replace history instead of stitching equal text. Data commits
//! precede the atomic current manifest, so interrupted tails remain invisible.
use crate::{
  AttachmentEvent,
  archive::{ArchivedPane, SessionArchive},
};
use ctmux_proto::{TerminalCheckpoint, TerminalHistoryManifest, TerminalHistoryRow, TerminalSize};
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

const MAX_REPLAY_BYTES: usize = 1024 * 1024;
const MAX_PROJECTION_ROWS: u64 = 10_000;
const MAX_HISTORY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const ARCHIVE_ID_PREFIX: &str = "archive:";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ReplayOutput {
  sequence_start: u64,
  data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PhysicalHistory {
  file: String,
  start: u64,
  end: u64,
  rows: u64,
  scrollback_limit: u64,
  partial_rows: u64,
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
  #[serde(default)]
  history_file: Option<String>,
  #[serde(default)]
  history_lines: u64,
  #[serde(default)]
  history_manifest: Option<TerminalHistoryManifest>,
  #[serde(default)]
  history_synced: bool,
  #[serde(default)]
  history_checkpoint: Option<TerminalCheckpoint>,
  #[serde(default)]
  replay: Vec<ReplayOutput>,
  #[serde(default)]
  replay_truncated: bool,
  #[serde(default)]
  physical_history: Option<PhysicalHistory>,
  #[serde(default)]
  preserved_archive: Option<String>,
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
  pub history_gap: bool,
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
      if !matches!(current.version, 1 | 2) {
        return Err(invalid("Unsupported local session cache version"));
      }
      if current.identity.terminal_size.columns == 0 || current.identity.terminal_size.rows == 0 {
        return Err(invalid("Invalid cached terminal dimensions"));
      }
      if let Some(file) = &current.history_file {
        validate_data_file(file)?;
      }
      if let Some(physical) = &current.physical_history {
        validate_data_file(&physical.file)?;
        if physical.start > physical.end
          || physical.rows > physical.scrollback_limit
          || physical.scrollback_limit > MAX_PROJECTION_ROWS
          || physical.partial_rows > physical.rows
        {
          return Err(invalid("Invalid cached history projection"));
        }
      }
      Ok(Some(current))
    }
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
    Err(error) => Err(error),
  }
}
fn history(directory: &Path, current: &Current, limit: Option<usize>) -> io::Result<Vec<String>> {
  if let Some(physical) = &current.physical_history {
    let mut lines = ctmux_proto::normalize_history_rows(&physical_rows(directory, physical)?);
    if let Some(limit) = limit {
      lines.drain(..lines.len().saturating_sub(limit));
    }
    return Ok(lines);
  }
  let file =
    match File::open(directory.join(current.history_file.as_deref().unwrap_or("history.jsonl"))) {
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
  let mut retained_bytes = 0;
  for line in BufReader::new(file.take(current.history_bytes)).lines() {
    let text: String = serde_json::from_str(&line?).map_err(io::Error::other)?;
    retained_bytes += text.len() + 1;
    result.push_back(text);
    while limit.is_some_and(|limit| {
      result.len() > limit || retained_bytes > ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES
    }) {
      if let Some(old) = result.pop_front() {
        retained_bytes -= old.len() + 1;
      }
    }
  }
  Ok(result.into())
}

fn validate_data_file(file: &str) -> io::Result<()> {
  if (!file.starts_with("history-") && !file.starts_with("rows-"))
    || Path::new(file)
      .extension()
      .is_none_or(|extension| extension != "jsonl")
    || Path::new(file).components().count() != 1
  {
    return Err(invalid("Invalid cache data filename"));
  }
  Ok(())
}

fn physical_rows(
  directory: &Path,
  physical: &PhysicalHistory,
) -> io::Result<Vec<TerminalHistoryRow>> {
  let mut file = File::open(directory.join(&physical.file))?;
  if file.metadata()?.len() < physical.end {
    return Err(invalid("Local history projection is incomplete"));
  }
  file.seek(SeekFrom::Start(physical.start))?;
  let rows: Vec<TerminalHistoryRow> = BufReader::new(file.take(physical.end - physical.start))
    .lines()
    .map(|line| serde_json::from_str(&line?).map_err(io::Error::other))
    .collect::<io::Result<_>>()?;
  if u64::try_from(rows.len()).map_err(io::Error::other)? != physical.rows {
    return Err(invalid("Local history projection row count differs"));
  }
  Ok(rows)
}

fn write_lines<T: Serialize>(
  directory: &Path,
  prefix: &str,
  lines: &[T],
) -> io::Result<(String, u64)> {
  let filename = format!("{prefix}-{}.jsonl", uuid::Uuid::new_v4());
  let path = directory.join(&filename);
  let result = (|| {
    let mut file = private_file(&path, false)?;
    let mut bytes = 0;
    for line in lines {
      let encoded = serde_json::to_vec(line).map_err(io::Error::other)?;
      file.write_all(&encoded)?;
      file.write_all(b"\n")?;
      bytes += u64::try_from(encoded.len() + 1).map_err(io::Error::other)?;
    }
    file.sync_all()?;
    Ok((filename, bytes))
  })();
  if result.is_err() {
    let _ignored = fs::remove_file(path);
  }
  result
}

fn replace_lines(directory: &Path, current: &mut Current, lines: &[String]) -> io::Result<()> {
  let mut bytes: usize = lines.iter().map(|line| line.len() + 1).sum();
  let mut start = 0;
  while (bytes > ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES
    || (lines.len() - start) as u64 > MAX_PROJECTION_ROWS)
    && start < lines.len()
  {
    bytes -= lines[start].len() + 1;
    start += 1;
  }
  let (file, history_bytes) = write_lines(directory, "history", &lines[start..])?;
  current.history_file = Some(file);
  current.history_bytes = history_bytes;
  current.history_lines = (lines.len() - start) as u64;
  current.physical_history = None;
  current.history_gap |= start > 0;
  Ok(())
}

fn replace_rows(
  directory: &Path,
  current: &mut Current,
  rows: &[TerminalHistoryRow],
  scrollback_limit: u64,
) -> io::Result<()> {
  if scrollback_limit > MAX_PROJECTION_ROWS || rows.len() as u64 > scrollback_limit {
    return Err(invalid("Unsupported history projection size"));
  }
  let (file, end) = write_lines(directory, "rows", rows)?;
  if end > MAX_HISTORY_FILE_BYTES {
    let _ignored = fs::remove_file(directory.join(file));
    return Err(invalid("History projection exceeds its storage bound"));
  }
  current.wrapped_prefix = crate::history::wrapped_history_prefix(rows);
  current.physical_history = Some(PhysicalHistory {
    file,
    start: 0,
    end,
    rows: rows.len() as u64,
    scrollback_limit,
    partial_rows: rows.iter().rev().take_while(|row| row.wrapped).count() as u64,
  });
  current.history_file = None;
  current.history_bytes = 0;
  current.history_lines = 0;
  Ok(())
}

fn append_lines(directory: &Path, current: &mut Current, lines: &[String]) -> io::Result<()> {
  if lines.is_empty() {
    return Ok(());
  }
  let Some(filename) = &current.history_file else {
    return replace_lines(directory, current, lines);
  };
  let mut file = private_file(&directory.join(filename), true)?;
  if file.metadata()?.len() < current.history_bytes {
    return Err(invalid("Local session history is incomplete"));
  }
  file.set_len(current.history_bytes)?;
  for line in lines {
    let bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    current.history_bytes += u64::try_from(bytes.len() + 1).map_err(io::Error::other)?;
    current.history_lines += 1;
  }
  file.sync_all()?;
  if current.history_bytes > ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES as u64
    || current.history_lines > MAX_PROJECTION_ROWS
  {
    let retained = history(
      directory,
      current,
      Some(MAX_PROJECTION_ROWS.try_into().map_err(io::Error::other)?),
    )?;
    current.history_gap |= (retained.len() as u64) < current.history_lines;
    replace_lines(directory, current, &retained)?;
  }
  Ok(())
}

fn cleanup_data_files(directory: &Path, current: &Current) -> io::Result<()> {
  for entry in fs::read_dir(directory)? {
    let entry = entry?;
    let filename = entry.file_name();
    let Some(filename) = filename.to_str() else {
      continue;
    };
    if entry.file_type()?.is_file()
      && (validate_data_file(filename).is_ok() || filename == "history.jsonl")
      && current.history_file.as_deref() != Some(filename)
      && current
        .physical_history
        .as_ref()
        .is_none_or(|physical| physical.file != filename)
    {
      fs::remove_file(entry.path())?;
    }
  }
  Ok(())
}

fn append_rows(
  directory: &Path,
  current: &mut Current,
  rows: &[TerminalHistoryRow],
  rebuild_prefix: bool,
) -> io::Result<()> {
  let Some(physical) = current.physical_history.as_mut() else {
    return Ok(());
  };
  let mut file = private_file(&directory.join(&physical.file), true)?;
  if file.metadata()?.len() < physical.end {
    return Err(invalid("Local history projection is incomplete"));
  }
  file.set_len(physical.end)?;
  for row in rows {
    let bytes = serde_json::to_vec(row).map_err(io::Error::other)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    physical.end += u64::try_from(bytes.len() + 1).map_err(io::Error::other)?;
    physical.rows += 1;
    physical.partial_rows = if row.wrapped {
      physical.partial_rows + 1
    } else {
      0
    };
  }
  file.sync_all()?;
  let excess = physical.rows.saturating_sub(physical.scrollback_limit);
  if excess > 0 {
    let partial_start = physical.rows - physical.partial_rows;
    file.seek(SeekFrom::Start(physical.start))?;
    let mut reader = BufReader::new(file.take(physical.end - physical.start));
    let mut dropped_prefix_bytes = 0;
    for index in 0..excess {
      let mut line = String::new();
      let bytes = reader.read_line(&mut line)?;
      if bytes == 0 {
        return Err(invalid("Local history projection is incomplete"));
      }
      let row: TerminalHistoryRow = serde_json::from_str(&line).map_err(io::Error::other)?;
      if index >= partial_start {
        dropped_prefix_bytes += row.text.len();
      }
      physical.start += u64::try_from(bytes).map_err(io::Error::other)?;
    }
    physical.rows -= excess;
    physical.partial_rows -= excess.saturating_sub(partial_start);
    if !rebuild_prefix {
      if dropped_prefix_bytes > current.wrapped_prefix.len() {
        return Err(invalid("Cached wrapped history prefix differs"));
      }
      current.wrapped_prefix.drain(..dropped_prefix_bytes);
    }
    current.history_gap = true;
  }
  if rebuild_prefix {
    current.wrapped_prefix =
      crate::history::wrapped_history_prefix(&physical_rows(directory, physical)?);
  }
  if physical.end > MAX_HISTORY_FILE_BYTES {
    let limit = physical.scrollback_limit;
    let retained = physical_rows(directory, physical)?;
    replace_rows(directory, current, &retained, limit)?;
  }
  Ok(())
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
    .reflow_cursor_line(false)
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

#[derive(Default)]
struct HistoryDelta {
  lines: std::collections::VecDeque<String>,
  rows: std::collections::VecDeque<TerminalHistoryRow>,
  line_bytes: usize,
  rebuild_prefix: bool,
}

fn bound_delta(current: &mut Current, delta: &mut HistoryDelta, row_limit: u64) {
  let discarded_rows = delta.rows.len() as u64 > row_limit;
  while delta.rows.len() as u64 > row_limit {
    delta.rows.pop_front();
  }
  if discarded_rows {
    delta.rebuild_prefix = true;
    current.wrapped_prefix = crate::history::wrapped_history_prefix(delta.rows.make_contiguous());
    current.history_gap = true;
  }
  while delta.lines.len() as u64 > MAX_PROJECTION_ROWS
    || delta.line_bytes > ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES
  {
    if let Some(line) = delta.lines.pop_front() {
      delta.line_bytes -= line.len() + 1;
    }
    current.history_gap = true;
  }
  if current.wrapped_prefix.len() > ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES {
    let mut start = current.wrapped_prefix.len() - ctmux_proto::MAX_NORMALIZED_HISTORY_BYTES;
    while !current.wrapped_prefix.is_char_boundary(start) {
      start += 1;
    }
    current.wrapped_prefix.drain(..start);
    current.history_gap = true;
  }
}

fn sync_history(
  current: &mut Current,
  directory: &Path,
  event: &AttachmentEvent,
) -> io::Result<bool> {
  let AttachmentEvent::HistorySynced {
    snapshot_id,
    checkpoint,
    history: snapshot,
    rows,
    scrollback_limit,
    history_gap,
  } = event
  else {
    return Err(invalid("Unexpected history synchronization event"));
  };
  let Some(manifest) = &current.history_manifest else {
    return Ok(false);
  };
  if current.history_synced || manifest.snapshot_id != *snapshot_id {
    return Ok(false);
  }
  if !snapshot.is_supported()
    || current.history_checkpoint.as_ref() != Some(checkpoint)
    || manifest.sequence != snapshot.sequence
    || manifest.generation != snapshot.generation
    || manifest.revision != snapshot.revision
    || manifest.scrollback_limit != *scrollback_limit
    || manifest.total_rows != rows.len() as u64
  {
    return Err(invalid("Completed history differs from its checkpoint"));
  }
  if current.replay_truncated {
    return Err(invalid(
      "History catch-up has a gap; request a new checkpoint",
    ));
  }
  let mut projected = crate::history::restore_projection(checkpoint, rows, *scrollback_limit)?;
  let mut pending = checkpoint.input_prefix.clone();
  let mut sequence = checkpoint.sequence;
  let mut evicted = false;
  for output in &current.replay {
    if output.sequence_start != sequence {
      return Err(invalid(
        "History catch-up has a gap; request a new checkpoint",
      ));
    }
    evicted |=
      crate::history::feed_projection_with_evictions(&mut projected, &mut pending, &output.data)?;
    sequence += u64::try_from(output.data.len()).map_err(io::Error::other)?;
  }
  if sequence != current.sequence
    || projected.dump() != current.payload
    || pending != current.pending_utf8
  {
    return Err(invalid("History catch-up differs from the current screen"));
  }
  let retained = crate::history::projection_rows(projected);
  current.history_gap = *history_gap || snapshot.truncated || manifest.truncated || evicted;
  // first_line locates the initial recent seed within this complete snapshot;
  // fetching the remaining rows fills that provisional gap.
  replace_rows(directory, current, &retained, *scrollback_limit)?;
  current.history_synced = true;
  current.replay.clear();
  Ok(true)
}

fn bound_legacy_history(directory: &Path, current: &mut Current) -> io::Result<()> {
  if current.version == 1 {
    let retained = history(
      directory,
      current,
      Some(MAX_PROJECTION_ROWS.try_into().map_err(io::Error::other)?),
    )?;
    current.version = 2;
    current.history_gap = true;
    replace_lines(directory, current, &retained)?;
  }
  Ok(())
}

fn update_current(
  current: &mut Current,
  vt: &mut avt::Vt,
  directory: &Path,
  event: &AttachmentEvent,
  has_previous: bool,
) -> io::Result<Option<HistoryDelta>> {
  let mut appended = HistoryDelta::default();
  match event {
    AttachmentEvent::Checkpoint {
      checkpoint,
      history: snapshot,
      history_manifest,
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
      if let Some(manifest) = history_manifest
        && (manifest.sequence != checkpoint.sequence
          || manifest.generation != snapshot.generation
          || manifest.revision != snapshot.revision
          || manifest.scrollback_limit > MAX_PROJECTION_ROWS)
      {
        return Err(invalid("Checkpoint history manifest differs"));
      }
      current.version = 2;
      current.history_gap = *history_gap || snapshot.truncated;
      replace_lines(directory, current, &snapshot.lines)?;
      current.identity.terminal_size = checkpoint.terminal_size.clone();
      current.pending_utf8.clone_from(&checkpoint.input_prefix);
      current.wrapped_prefix.clear();
      current.history_manifest.clone_from(history_manifest);
      current.history_synced = history_manifest.is_none();
      current.history_checkpoint = Some(checkpoint.clone());
      current.replay.clear();
      current.replay_truncated = false;
      current.sequence = checkpoint.sequence;
      *vt = emulator(&checkpoint.terminal_size);
      drop(vt.feed_str(std::str::from_utf8(&checkpoint.payload).map_err(io::Error::other)?));
    }
    AttachmentEvent::HistorySynced { .. } => {
      if !sync_history(current, directory, event)? {
        return Ok(None);
      }
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
      bound_legacy_history(directory, current)?;
      appended = update_output(current, vt, directory, *sequence_start, *sequence_end, data)?;
    }
    AttachmentEvent::PtyGeometryChanged {
      terminal_size,
      observed_sequence,
    } => {
      if *observed_sequence < current.sequence {
        return Ok(None);
      }
      if terminal_size.columns == 0 || terminal_size.rows == 0 {
        return Err(invalid("Invalid terminal dimensions"));
      }
      bound_legacy_history(directory, current)?;
      let rows: Vec<_> = vt
        .resize(
          usize::from(terminal_size.columns),
          usize::from(terminal_size.rows),
        )
        .scrollback
        .collect();
      let mut unwrapper = avt::util::TextUnwrapper::new();
      appended.rows = rows
        .iter()
        .map(|line| TerminalHistoryRow {
          text: line.text(),
          wrapped: unwrapper.push(line).is_none(),
        })
        .collect();
      appended.lines = collect(rows.into_iter(), &mut current.wrapped_prefix).into();
      appended.line_bytes = appended.lines.iter().map(|line| line.len() + 1).sum();
      bound_delta(current, &mut appended, MAX_PROJECTION_ROWS);
      current.history_manifest = None;
      current.history_checkpoint = None;
      current.replay.clear();
      current.replay_truncated = true;
      current.history_gap = true;
      current.identity.terminal_size = terminal_size.clone();
    }
    _ => unreachable!(),
  }
  Ok(Some(appended))
}

fn update_output(
  current: &mut Current,
  vt: &mut avt::Vt,
  directory: &Path,
  start: u64,
  end: u64,
  data: &[u8],
) -> io::Result<HistoryDelta> {
  let mut appended = HistoryDelta::default();
  if start > current.sequence {
    return Err(invalid(
      "Local session output has a gap; reconnect to restore a checkpoint",
    ));
  }
  let offset = usize::try_from(current.sequence - start).map_err(io::Error::other)?;
  if end - start != u64::try_from(data.len()).map_err(io::Error::other)? || offset > data.len() {
    return Err(invalid("Invalid local output range"));
  }
  let mut control_parser = avt::parser::Parser::default();
  for character in current.payload.chars() {
    let _function = control_parser.feed(character);
  }
  retain_replay(current, &data[offset..]);
  current.pending_utf8.extend_from_slice(&data[offset..]);
  let row_limit = current.physical_history.as_ref().map_or_else(
    || {
      current
        .history_manifest
        .as_ref()
        .map_or(MAX_PROJECTION_ROWS, |manifest| manifest.scrollback_limit)
    },
    |physical| physical.scrollback_limit,
  );
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
    let mut clear = false;
    for character in text.chars() {
      clear |= matches!(
        control_parser.feed(character),
        Some(
          avt::parser::Function::Ed(avt::parser::EdScope::SavedLines) | avt::parser::Function::Ris
        )
      );
    }
    for chunk in crate::history::projection_chunks(&text, vt.size().0) {
      let rows: Vec<_> = vt.feed_str(chunk).scrollback.collect();
      let mut unwrapper = avt::util::TextUnwrapper::new();
      appended
        .rows
        .extend(rows.iter().map(|line| TerminalHistoryRow {
          text: line.text(),
          wrapped: unwrapper.push(line).is_none(),
        }));
      let lines = collect(rows.into_iter(), &mut current.wrapped_prefix);
      appended.line_bytes += lines.iter().map(|line| line.len() + 1).sum::<usize>();
      appended.lines.extend(lines);
      bound_delta(current, &mut appended, row_limit);
    }
    if clear {
      current.history_manifest = None;
      current.history_checkpoint = None;
      current.history_synced = false;
      current.replay.clear();
      current.replay_truncated = true;
      // A following checkpoint supplies the new generation, including any
      // history produced after the clear within this same output batch.
      current.history_gap = true;
      current.wrapped_prefix.clear();
      replace_lines(directory, current, &[])?;
      appended.lines.clear();
      appended.rows.clear();
      appended.line_bytes = 0;
    }
    current.pending_utf8.drain(..consumed);
  }
  current.sequence = end;
  Ok(appended)
}

fn retain_replay(current: &mut Current, bytes: &[u8]) {
  if current.history_manifest.is_none() || current.history_synced || current.replay_truncated {
    return;
  }
  let retained_bytes: usize = current.replay.iter().map(|output| output.data.len()).sum();
  if retained_bytes.saturating_add(bytes.len()) > MAX_REPLAY_BYTES {
    current.replay.clear();
    current.replay_truncated = true;
    current.history_gap = true;
  } else if let Some(previous) = current.replay.last_mut()
    && previous.sequence_start + previous.data.len() as u64 == current.sequence
  {
    // Fragmented PTY reads must not create an unbounded number of entries.
    previous.data.extend_from_slice(bytes);
  } else {
    current.replay.push(ReplayOutput {
      sequence_start: current.sequence,
      data: bytes.to_vec(),
    });
  }
}

fn copy_record(source: &Path, destination: &Path) -> io::Result<()> {
  private_directory(destination)?;
  let current = read_current(source)?;
  for entry in fs::read_dir(source)? {
    let entry = entry?;
    let target = destination.join(entry.file_name());
    if entry.file_type()?.is_dir() {
      copy_record(&entry.path(), &target)?;
    } else if entry.file_name() == "current.json"
      || current.as_ref().is_some_and(|current| {
        let name = entry.file_name();
        current
          .history_file
          .as_ref()
          .is_some_and(|filename| name == filename.as_str())
          || (current.version == 1 && name == "history.jsonl")
          || current
            .physical_history
            .as_ref()
            .is_some_and(|physical| name == physical.file.as_str())
      })
    {
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
    if path.extension().is_some_and(|extension| extension == "tmp") {
      fs::remove_dir_all(path)?;
      continue;
    }
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
  // Preserve the complete old session before the first bounded cache commit. The
  // separate ID is read-only: neither loading nor applying can turn it into a
  // reconnect target. Original pane names, reasons, and timestamps stay intact.
  fn preserve_legacy(&self, root: &Path, identity: &CacheIdentity) -> io::Result<()> {
    let mut panes = Vec::new();
    for entry in fs::read_dir(root)? {
      let path = entry?.path();
      if let Some(current) = read_current(&path)? {
        panes.push((path, current));
      }
    }
    if !panes.iter().any(|(_, current)| current.version == 1) {
      return Ok(());
    }
    if panes
      .iter()
      .filter_map(|(_, current)| current.preserved_archive.as_deref())
      .any(|id| {
        id.starts_with(ARCHIVE_ID_PREFIX)
          && self.session_path(&identity.host_key, id, true).exists()
      })
    {
      return Ok(());
    }
    panes.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    for (path, _) in &panes {
      digest.update(fs::read(path.join("current.json"))?);
    }
    let archive_id = format!(
      "{ARCHIVE_ID_PREFIX}{}:{:x}",
      identity.session_id,
      digest.finalize()
    );
    let archived = self.session_path(&identity.host_key, &archive_id, true);
    let parent = archived
      .parent()
      .ok_or_else(|| invalid("Missing archive parent"))?;
    private_directory(parent)?;
    let temporary = archived.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
      if !archived.exists() {
        copy_record(root, &temporary)?;
        for entry in fs::read_dir(&temporary)? {
          let directory = entry?.path();
          if let Some(mut current) = read_current(&directory)? {
            current.identity.session_id.clone_from(&archive_id);
            atomic_json(&directory.join("current.json"), &current)?;
          }
        }
        fs::rename(&temporary, &archived)?;
        sync_directory(parent)?;
      }
      for (directory, mut current) in panes {
        current.preserved_archive = Some(archive_id.clone());
        atomic_json(&directory.join("current.json"), &current)?;
      }
      Ok(())
    })();
    if result.is_err() {
      let _ignored = fs::remove_dir_all(temporary);
    }
    result
  }
  /// Saves the bounded history projection and latest screen before acknowledgement.
  /// # Errors
  /// Returns storage errors or an invalid/gapped output sequence.
  pub fn apply(&self, identity: &CacheIdentity, event: &AttachmentEvent) -> io::Result<()> {
    if !matches!(
      event,
      AttachmentEvent::Checkpoint { .. }
        | AttachmentEvent::HistorySynced { .. }
        | AttachmentEvent::Output { .. }
        | AttachmentEvent::PtyGeometryChanged { .. }
    ) {
      return Ok(());
    }
    if identity.session_id.starts_with(ARCHIVE_ID_PREFIX) {
      return Err(invalid("Read-only archive cannot be resumed"));
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
    if previous
      .as_ref()
      .is_some_and(|current| current.version == 1)
    {
      self.preserve_legacy(&root, identity)?;
    }
    let mut current = previous.clone().unwrap_or_else(|| Current {
      version: 2,
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
      history_file: None,
      history_lines: 0,
      history_manifest: None,
      history_synced: false,
      history_checkpoint: None,
      replay: Vec::new(),
      replay_truncated: false,
      physical_history: None,
      preserved_archive: None,
    });
    let mut vt = emulator(&current.identity.terminal_size);
    drop(vt.feed_str(&current.payload));
    let Some(mut appended) =
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
    if current.physical_history.is_some() {
      append_rows(
        &directory,
        &mut current,
        appended.rows.make_contiguous(),
        appended.rebuild_prefix,
      )?;
    } else {
      append_lines(&directory, &mut current, appended.lines.make_contiguous())?;
    }
    atomic_json(&directory.join("current.json"), &current)?;
    cleanup_data_files(&directory, &current)
  }
  /// # Errors
  /// Returns storage or corrupt-record errors.
  pub fn load(
    &self,
    host_key: &str,
    session_id: &str,
    terminal_id: Option<&str>,
  ) -> io::Result<Option<CachedPresentation>> {
    if session_id.starts_with(ARCHIVE_ID_PREFIX) {
      return Ok(None);
    }
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
        // Older archives may retain more; the renderer still receives a bounded tail.
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
          history_gap: current.history_gap
            || (current.history_manifest.is_some() && !current.history_synced),
        }));
      }
    }
    Ok(None)
  }
  /// Mark dropped local output as incomplete without changing the saved screen.
  /// A fresh authoritative checkpoint is required before history can resume.
  ///
  /// # Errors
  /// Returns storage errors or an error when no live snapshot has been saved.
  pub fn mark_history_gap(&self, identity: &CacheIdentity) -> io::Result<()> {
    if identity.session_id.starts_with(ARCHIVE_ID_PREFIX) {
      return Err(invalid("Read-only archive cannot be resumed"));
    }
    let _lock = self.lock()?;
    let directory = self
      .session_path(&identity.host_key, &identity.session_id, false)
      .join(key(&identity.terminal_id));
    let mut current = read_current(&directory)?.ok_or_else(|| {
      io::Error::new(
        io::ErrorKind::NotFound,
        "No live snapshot is available to mark incomplete",
      )
    })?;
    current.history_gap = true;
    current.history_manifest = None;
    current.history_checkpoint = None;
    current.history_synced = false;
    current.replay.clear();
    current.replay_truncated = true;
    atomic_json(&directory.join("current.json"), &current)
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
          history_gap: current.history_gap
            || (current.history_manifest.is_some() && !current.history_synced),
        });
      }
      if let Some(archive) = archive {
        result.push(archive);
      }
    }
    result.sort_by_key(|archive| std::cmp::Reverse(archive.archived_at_ms));
    Ok(result)
  }
  /// Reads a bounded page. Legacy transcripts stream directly from their file;
  /// new records normalize only their bounded physical history window.
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
    let materialized = if current.version == 2 {
      let mut bytes = Vec::new();
      for line in history(&directory, &current, None)? {
        serde_json::to_writer(&mut bytes, &line).map_err(io::Error::other)?;
        bytes.push(b'\n');
      }
      Some(bytes)
    } else {
      None
    };
    let history_bytes = materialized
      .as_ref()
      .map_or(current.history_bytes, |bytes| bytes.len() as u64);
    if offset > history_bytes {
      return Err(invalid("Invalid archive offset"));
    }
    let mut position = offset;
    let mut lines = Vec::new();
    if offset < history_bytes {
      let mut reader: Box<dyn BufRead> = if let Some(bytes) = materialized {
        let mut cursor = io::Cursor::new(bytes);
        cursor.seek(SeekFrom::Start(offset))?;
        Box::new(BufReader::new(cursor))
      } else {
        let mut file = File::open(directory.join("history.jsonl"))?;
        if file.metadata()?.len() < history_bytes {
          return Err(invalid("Local session history is incomplete"));
        }
        file.seek(SeekFrom::Start(offset))?;
        Box::new(BufReader::new(file.take(history_bytes - offset)))
      };
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
    let next_offset = (position < history_bytes).then_some(position);
    if next_offset.is_none() {
      lines.extend(archive_screen_lines(&current));
    }
    Ok(Some(ArchivePage {
      lines,
      next_offset,
      history_gap: current.history_gap
        || (current.history_manifest.is_some() && !current.history_synced),
    }))
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
          history_manifest: None,
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
    fn directory(&self, archived: bool) -> PathBuf {
      self
        .store
        .session_path("local", "session", archived)
        .join(key("pane"))
    }
    fn current(&self) -> io::Result<Current> {
      read_current(&self.directory(false))?.ok_or_else(|| invalid("Missing current"))
    }
    fn archive_lines(&self, session_id: &str, terminal_id: &str) -> io::Result<Vec<String>> {
      let mut offset = 0;
      let mut lines = Vec::new();
      loop {
        let page = self
          .store
          .read_archive("local", session_id, terminal_id, offset)?
          .ok_or_else(|| invalid("Missing archive page"))?;
        lines.extend(page.lines);
        if let Some(next) = page.next_offset {
          assert!(next > offset);
          offset = next;
        } else {
          return Ok(lines);
        }
      }
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

  fn snapshot_events(
    source: &avt::Vt,
    sequence: u64,
    snapshot_id: &str,
    generation: u64,
    revision: u64,
    scrollback_limit: u64,
    input_prefix: Vec<u8>,
  ) -> io::Result<(AttachmentEvent, AttachmentEvent)> {
    let mut unwrapper = avt::util::TextUnwrapper::new();
    let rows: Vec<_> = source
      .lines()
      .take(source.lines().count() - source.size().1)
      .map(|line| TerminalHistoryRow {
        text: line.text(),
        wrapped: unwrapper.push(line).is_none(),
      })
      .collect();
    let lines = ctmux_proto::normalize_history_rows(&rows);
    let history = ctmux_proto::TerminalHistorySnapshot {
      format: ctmux_proto::TERMINAL_HISTORY_FORMAT.into(),
      format_version: 1,
      sequence,
      generation,
      revision,
      retained_bytes: lines.iter().map(|line| line.len() as u64 + 1).sum(),
      truncated: false,
      lines,
    };
    let checkpoint = TerminalCheckpoint {
      format: ctmux_proto::TERMINAL_CHECKPOINT_FORMAT.into(),
      format_version: 1,
      sequence,
      terminal_size: TerminalSize {
        columns: u16::try_from(source.size().0).map_err(io::Error::other)?,
        rows: u16::try_from(source.size().1).map_err(io::Error::other)?,
        ..TerminalSize::default()
      },
      payload: source.dump().into_bytes(),
      input_prefix,
    };
    let mut encoded = Vec::new();
    for row in &rows {
      serde_json::to_writer(&mut encoded, row).map_err(io::Error::other)?;
      encoded.push(b'\n');
    }
    let manifest = TerminalHistoryManifest {
      snapshot_id: snapshot_id.into(),
      sequence,
      generation,
      revision,
      total_rows: rows.len() as u64,
      total_bytes: encoded.len() as u64,
      total_lines: history.lines.len() as u64,
      first_line: 0,
      truncated: false,
      content_hash: key(std::str::from_utf8(&encoded).map_err(io::Error::other)?),
      scrollback_limit,
    };
    let mut seed = history.clone();
    seed.lines.drain(..seed.lines.len().saturating_sub(1));
    Ok((
      AttachmentEvent::Checkpoint {
        checkpoint: checkpoint.clone(),
        history: seed,
        history_manifest: Some(manifest),
        history_gap: false,
      },
      AttachmentEvent::HistorySynced {
        snapshot_id: snapshot_id.into(),
        checkpoint,
        history,
        rows,
        scrollback_limit,
        history_gap: false,
      },
    ))
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
      .directory(false)
      .join(fixture.current()?.history_file.unwrap());
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
    let mut source = avt::Vt::builder().size(3, 2).scrollback_limit(10).build();
    drop(source.feed_str("abcdefghi"));
    let (checkpoint, synced) = snapshot_events(&source, sequence, "wrapped", 0, 1, 10, vec![])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    fixture.store.apply(&fixture.identity, &synced)?;
    fixture.output(sequence, "\r\nnext\r\nlast")?;
    assert_eq!(fixture.presentation()?.history[0], "abcdefghi");
    Ok(())
  }

  #[test]
  fn repeated_lines_replace_the_remote_window_on_every_reconnect() -> io::Result<()> {
    let fixture = Fixture::new();
    let text = "same\r\nsame\r\nsame\r\nsame\r\nsame";
    let mut source = avt::Vt::builder().size(20, 2).scrollback_limit(2).build();
    drop(source.feed_str(text));
    for id in ["first", "reconnected"] {
      let (checkpoint, synced) = snapshot_events(&source, text.len() as u64, id, 0, 1, 2, vec![])?;
      fixture.store.apply(&fixture.identity, &checkpoint)?;
      assert_eq!(fixture.presentation()?.history, ["same"]);
      fixture.store.apply(&fixture.identity, &synced)?;
      assert_eq!(fixture.presentation()?.history, ["same", "same"]);
    }
    Ok(())
  }

  #[test]
  fn equal_sequence_resize_and_new_generation_discard_stale_background_history() -> io::Result<()> {
    let fixture = Fixture::new();
    let text = "a\r\nb\r\nc\r\nd";
    let sequence = text.len() as u64;
    let mut source = avt::Vt::builder().size(20, 2).scrollback_limit(10).build();
    drop(source.feed_str(text));
    let (old_checkpoint, old_synced) = snapshot_events(&source, sequence, "old", 0, 1, 10, vec![])?;
    fixture.store.apply(&fixture.identity, &old_checkpoint)?;
    fixture.store.apply(&fixture.identity, &old_synced)?;
    assert_eq!(fixture.presentation()?.history, ["a", "b"]);
    drop(source.resize(20, 4));
    let (resized_checkpoint, resized_synced) =
      snapshot_events(&source, sequence, "resized", 0, 2, 10, vec![])?;
    fixture
      .store
      .apply(&fixture.identity, &resized_checkpoint)?;
    fixture.store.apply(&fixture.identity, &old_synced)?;
    fixture.store.apply(&fixture.identity, &resized_synced)?;
    assert_eq!(fixture.presentation()?.history, Vec::<String>::new());
    assert_eq!(
      fixture.presentation()?.checkpoint.payload,
      source.dump().into_bytes()
    );
    assert_eq!(fixture.presentation()?.checkpoint.terminal_size.rows, 4);
    drop(source.feed_str("\x1b[3J"));
    let (cleared_checkpoint, cleared_synced) =
      snapshot_events(&source, sequence + 4, "cleared", 1, 3, 10, vec![])?;
    fixture
      .store
      .apply(&fixture.identity, &cleared_checkpoint)?;
    fixture.store.apply(&fixture.identity, &resized_synced)?;
    fixture.store.apply(&fixture.identity, &cleared_synced)?;
    assert_eq!(fixture.presentation()?.history, Vec::<String>::new());
    assert_eq!(fixture.current()?.history_manifest.unwrap().generation, 1);
    Ok(())
  }

  #[test]
  fn background_history_replays_newer_output_without_regressing_the_screen() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    let mut source = avt::Vt::builder().size(3, 2).scrollback_limit(20).build();
    drop(source.feed_str("abcdefghi"));
    let (checkpoint, synced) = snapshot_events(&source, 10, "partial", 0, 1, 20, vec![0xc3])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    let newer = "ér\r\nnext\r\nlast";
    let continuation = &newer.as_bytes()[1..];
    fixture.store.apply(
      &fixture.identity,
      &AttachmentEvent::Output {
        sequence_start: 10,
        sequence_end: 10 + continuation.len() as u64,
        data: continuation.to_vec(),
      },
    )?;
    drop(source.feed_str(newer));
    let before = fixture.presentation()?;
    fixture.store.apply(&fixture.identity, &synced)?;
    let after = fixture.presentation()?;
    assert_eq!(after.checkpoint.sequence, before.checkpoint.sequence);
    assert_eq!(after.checkpoint.payload, before.checkpoint.payload);
    assert_eq!(after.checkpoint.payload, source.dump().into_bytes());
    assert_eq!(after.checkpoint.input_prefix, Vec::<u8>::new());
    let rows = crate::history::projection_rows(source);
    let mut expected = ctmux_proto::normalize_history_rows(&rows);
    let prefix = crate::history::wrapped_history_prefix(&rows);
    if !prefix.is_empty() {
      expected.push(prefix);
    }
    assert_eq!(after.history, expected);
    assert!(
      after
        .history
        .iter()
        .any(|line| line.starts_with("abcdefghi"))
    );
    assert!(fixture.current()?.replay.is_empty());
    assert!(fixture.current()?.history_synced);
    Ok(())
  }

  #[test]
  fn a_synced_cache_mirrors_the_bounded_physical_window_and_rolls_back_uncommitted_rows()
  -> io::Result<()> {
    let fixture = Fixture::new();
    let initial = "old\r\na\r\nb\r\nc";
    let mut source = avt::Vt::builder().size(20, 2).scrollback_limit(2).build();
    drop(source.feed_str(initial));
    let (checkpoint, synced) =
      snapshot_events(&source, initial.len() as u64, "bounded", 0, 1, 2, vec![])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    fixture.store.apply(&fixture.identity, &synced)?;
    let current = fixture.current()?;
    let physical = current.physical_history.as_ref().unwrap();
    let path = fixture.directory(false).join(&physical.file);
    private_file(&path, true)?.write_all(b"{\"text\":\"uncommitted\",\"wrapped\":false}\n")?;
    assert_eq!(fixture.presentation()?.history, ["old", "a"]);
    let sequence = fixture.output(initial.len() as u64, "\r\nd\r\ne\r\nf\r\ng")?;
    drop(source.feed_str("\r\nd\r\ne\r\nf\r\ng"));
    assert_eq!(fixture.presentation()?.checkpoint.sequence, sequence);
    assert_eq!(fixture.presentation()?.history, ["d", "e"]);
    assert!(fixture.presentation()?.history_gap);
    assert_eq!(fixture.current()?.physical_history.unwrap().rows, 2);
    assert_eq!(
      fixture.presentation()?.history,
      ctmux_proto::normalize_history_rows(&crate::history::projection_rows(source))
    );
    assert!(!fs::read_to_string(path)?.contains("uncommitted"));
    assert_eq!(fs::read_dir(fixture.directory(false))?.count(), 2);
    assert!(fixture.archive()?.history_gap);
    Ok(())
  }

  #[test]
  fn an_overrun_background_transfer_does_not_publish_guessed_history() -> io::Result<()> {
    let fixture = Fixture::new();
    let source = avt::Vt::builder().size(20, 2).scrollback_limit(2).build();
    let (checkpoint, synced) = snapshot_events(&source, 0, "slow", 0, 1, 2, vec![])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    fixture.output(0, &"x".repeat(MAX_REPLAY_BYTES + 1))?;
    let before = fixture.presentation()?;
    assert!(before.history_gap);
    assert!(fixture.store.apply(&fixture.identity, &synced).is_err());
    let after = fixture.presentation()?;
    assert_eq!(after.checkpoint, before.checkpoint);
    assert_eq!(after.history, before.history);
    Ok(())
  }

  #[test]
  fn a_wrapped_burst_rebuilds_only_the_retained_physical_prefix() -> io::Result<()> {
    let mut fixture = Fixture::new();
    fixture.identity.terminal_size.columns = 3;
    let mut source = avt::Vt::builder().size(3, 2).scrollback_limit(4).build();
    drop(source.feed_str("abcdefghi"));
    let (checkpoint, synced) = snapshot_events(&source, 9, "burst", 0, 1, 4, vec![])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    fixture.store.apply(&fixture.identity, &synced)?;
    fixture.output(9, &"x".repeat(512))?;
    drop(source.feed_str(&"x".repeat(512)));
    let rows = crate::history::projection_rows(source);
    assert_eq!(
      fixture.current()?.wrapped_prefix,
      crate::history::wrapped_history_prefix(&rows)
    );
    assert_eq!(
      physical_rows(
        &fixture.directory(false),
        fixture.current()?.physical_history.as_ref().unwrap()
      )?,
      rows
    );
    assert!(fixture.presentation()?.history_gap);
    Ok(())
  }

  #[test]
  fn dropped_output_is_marked_incomplete_until_a_fresh_checkpoint() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "screen", &["older"])?;
    let before = fixture.presentation()?;
    fixture.store.mark_history_gap(&fixture.identity)?;
    let marked = fixture.presentation()?;
    assert!(marked.history_gap);
    assert_eq!(marked.checkpoint, before.checkpoint);
    assert_eq!(marked.history, before.history);
    assert!(fixture.archive()?.history_gap);
    fixture.checkpoint(0, "fresh screen", &["authoritative"])?;
    assert!(!fixture.presentation()?.history_gap);
    assert_eq!(fixture.presentation()?.history, ["authoritative"]);
    Ok(())
  }

  #[test]
  fn a_fresh_checkpoint_recovers_corrupt_projection_storage_without_overwriting_on_failure()
  -> io::Result<()> {
    let fixture = Fixture::new();
    let initial = "older\r\na\r\nb";
    let mut source = avt::Vt::builder().size(20, 2).scrollback_limit(10).build();
    drop(source.feed_str(initial));
    let (checkpoint, synced) =
      snapshot_events(&source, initial.len() as u64, "initial", 0, 1, 10, vec![])?;
    fixture.store.apply(&fixture.identity, &checkpoint)?;
    assert!(fixture.presentation()?.history_gap);
    fixture.store.apply(&fixture.identity, &synced)?;
    assert!(!fixture.presentation()?.history_gap);
    let before = fs::read(fixture.directory(false).join("current.json"))?;
    let physical = fixture.current()?.physical_history.unwrap();
    private_file(&fixture.directory(false).join(physical.file), false)?
      .set_len(physical.end - 1)?;
    assert!(fixture.presentation().is_err());
    assert!(fixture.output(initial.len() as u64, "\r\nnew").is_err());
    assert_eq!(
      fs::read(fixture.directory(false).join("current.json"))?,
      before
    );
    let (fresh_checkpoint, fresh_synced) =
      snapshot_events(&source, initial.len() as u64, "recovered", 0, 2, 10, vec![])?;
    fixture.store.apply(&fixture.identity, &fresh_checkpoint)?;
    fixture.store.apply(&fixture.identity, &fresh_synced)?;
    assert_eq!(fixture.presentation()?.history, ["older"]);
    assert_eq!(
      fixture.presentation()?.checkpoint.payload,
      source.dump().into_bytes()
    );
    Ok(())
  }

  #[test]
  fn migrating_a_legacy_archive_preserves_its_full_read_only_copy() -> io::Result<()> {
    let fixture = Fixture::new();
    fixture.checkpoint(0, "old screen", &[])?;
    let directory = fixture.directory(false);
    let lines: Vec<_> = (0..12_000).map(|index| format!("old {index}")).collect();
    let (file, history_bytes) = write_lines(&directory, "history", &lines)?;
    fs::rename(directory.join(file), directory.join("history.jsonl"))?;
    let mut current = fixture.current()?;
    current.version = 1;
    current.history_file = None;
    current.history_bytes = history_bytes;
    current.history_lines = 0;
    atomic_json(&directory.join("current.json"), &current)?;
    let second_identity = CacheIdentity {
      terminal_id: "second".into(),
      primary: false,
      ..fixture.identity.clone()
    };
    let second_directory = fixture
      .store
      .session_path("local", "session", false)
      .join(key("second"));
    copy_record(&directory, &second_directory)?;
    let mut second_current = read_current(&second_directory)?.unwrap();
    second_current.identity = second_identity.clone();
    atomic_json(&second_directory.join("current.json"), &second_current)?;
    fixture
      .store
      .archive("local", "session", "Original reason")?;
    let mut current = read_current(&fixture.directory(true))?.unwrap();
    current.archived_at_ms = 123;
    atomic_json(&fixture.directory(true).join("current.json"), &current)?;
    let second_archived = fixture
      .store
      .session_path("local", "session", true)
      .join(key("second"));
    second_current.archived_at_ms = 123;
    second_current.reason = Some("Original reason".into());
    atomic_json(&second_archived.join("current.json"), &second_current)?;
    fixture.checkpoint(0, "new screen", &["recent"])?;
    let source = avt::Vt::builder().size(20, 2).scrollback_limit(10).build();
    let (second_checkpoint, _) = snapshot_events(&source, 0, "second", 0, 1, 10, vec![])?;
    fixture.store.apply(&second_identity, &second_checkpoint)?;
    fixture.store.archive("local", "session", "New close")?;
    let archives = fixture.store.archives()?;
    assert_eq!(archives.len(), 2);
    let preserved = archives
      .iter()
      .find(|archive| archive.session_id.starts_with(ARCHIVE_ID_PREFIX))
      .unwrap();
    assert_eq!(preserved.name, fixture.identity.name);
    assert_eq!(preserved.archived_at_ms, 123);
    assert_eq!(preserved.terminals.len(), 2);
    assert_eq!(preserved.terminals[0].reason, "Original reason");
    let saved = fixture.archive_lines(&preserved.session_id, "pane")?;
    assert_eq!(&saved[..lines.len()], lines);
    assert_eq!(saved.last().unwrap(), "old screen");
    assert_eq!(saved.len(), 12_001);
    assert!(
      fixture
        .store
        .load("local", &preserved.session_id, None)?
        .is_none()
    );
    let readonly_identity = CacheIdentity {
      session_id: preserved.session_id.clone(),
      ..fixture.identity.clone()
    };
    assert!(
      fixture
        .store
        .apply(
          &readonly_identity,
          &AttachmentEvent::Output {
            sequence_start: 0,
            sequence_end: 1,
            data: vec![b'x'],
          }
        )
        .is_err()
    );
    assert_eq!(
      fixture
        .store
        .read_archive("local", "session", "pane", 0)?
        .unwrap()
        .lines,
      ["recent", "new screen"]
    );
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
