use super::{Level, Outcome, Record, Stream};
use serde::Serialize;
use uuid::Uuid;

mod audit;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SEGMENT_BYTES: u64 = 5 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 2048;

#[derive(Debug, Serialize)]
pub struct History {
  pub records: Vec<Record>,
  pub complete: bool,
  pub warning: Option<&'static str>,
}

#[derive(Debug)]
pub struct Store {
  directory: PathBuf,
  segment_bytes: u64,
}

impl Store {
  /// Select a history directory; no I/O occurs until append/read.
  #[must_use]
  pub fn new(directory: PathBuf) -> Self {
    Self {
      directory,
      segment_bytes: SEGMENT_BYTES,
    }
  }

  #[must_use]
  pub fn directory(&self) -> &Path {
    &self.directory
  }

  pub(super) fn log_path(&self, run: Uuid, index: usize) -> PathBuf {
    self.directory.join("logs").join(if index == 0 {
      format!("{run}.log")
    } else {
      format!("{run}.{index}.log")
    })
  }

  pub(super) fn append(&self, stream: Stream, record: &Record) -> io::Result<()> {
    if !record.valid() {
      return Err(io::Error::other("invalid history record"));
    }
    prepare_directory(&self.directory)?;
    if stream == Stream::Audit {
      return self.append_audit(record);
    }
    let mut encoded = record.log_line()?.into_bytes();
    encoded.push(b'\n');
    if encoded.len() > MAX_RECORD_BYTES {
      return Err(io::Error::other("history record exceeds size limit"));
    }
    prepare_directory(&self.directory.join("logs"))?;
    let _lock = self.lock(record.run_id, true)?;
    let path = self.log_path(record.run_id, 0);
    let mut file = open(&path, true)?;
    let size = file.metadata()?.len();
    if size > self.segment_bytes {
      return Err(io::Error::other("history segment exceeds size limit"));
    }
    // Reserve one byte to separate a potentially interrupted final record.
    if size + encoded.len() as u64 + 1 > self.segment_bytes {
      drop(file);
      for index in (1..4).rev() {
        let previous = self.log_path(record.run_id, index - 1);
        let destination = self.log_path(record.run_id, index);
        if exists(&previous)? {
          let _checked = open(&previous, false)?;
          if exists(&destination)? {
            let _checked = open(&destination, false)?;
            fs::remove_file(&destination)?;
          }
          fs::rename(previous, destination)?;
        }
      }
      file = open(&path, true)?;
    }
    // A killed writer may leave a partial line. Terminate it before appending
    // so it cannot consume the next valid record during review.
    if file.metadata()?.len() > 0 {
      file.seek(SeekFrom::End(-1))?;
      let mut last = [0];
      file.read_exact(&mut last)?;
      if last[0] != b'\n' {
        file.write_all(b"\n")?;
      }
    }
    file.write_all(&encoded)?;
    file.sync_data()
  }

  /// Read the newest matching records in audit append order or diagnostic time order.
  /// Missing history is empty; corruption is reported rather than hidden.
  ///
  /// # Errors
  /// Returns errors for unsafe paths, unavailable locks, oversized files, or I/O.
  pub fn read(&self, stream: Stream, limit: usize, failed_only: bool) -> io::Result<History> {
    self.read_run(stream, limit, failed_only, None)
  }

  /// Select one process run, or combine all runs. This never creates missing history.
  ///
  /// # Errors
  /// Returns errors for invalid limits, unsafe paths, contention, or I/O.
  pub fn read_run(
    &self,
    stream: Stream,
    limit: usize,
    failed_only: bool,
    run: Option<Uuid>,
  ) -> io::Result<History> {
    self.read_filtered(stream, limit, failed_only, run, None)
  }

  /// Read stored records at or above a minimum severity, before applying the limit.
  ///
  /// # Errors
  /// Returns errors for invalid limits, unsafe paths, contention, or I/O.
  pub fn read_filtered(
    &self,
    stream: Stream,
    limit: usize,
    failed_only: bool,
    run: Option<Uuid>,
    level: Option<Level>,
  ) -> io::Result<History> {
    if !(1..=10_000).contains(&limit) {
      return Err(io::Error::other(
        "history limit must be between 1 and 10000",
      ));
    }
    let mut history = History {
      records: Vec::new(),
      complete: true,
      warning: None,
    };
    if !exists(&self.directory)? {
      return Ok(history);
    }
    check_directory(&self.directory, true)?;
    check_ancestors(&self.directory)?;
    if stream == Stream::Audit {
      return self.read_audit(limit, failed_only, run, level);
    }
    for index in 0..4 {
      let name = if index == 0 {
        "logs.jsonl".into()
      } else {
        format!("logs.{index}.jsonl")
      };
      if exists(&self.directory.join(name))? {
        history.complete = false;
        history.warning = Some(
          "Legacy diagnostic JSONL files remain in the history directory; they are not included in per-run logs.",
        );
      }
    }
    let directory = self.directory.join("logs");
    if !exists(&directory)? {
      return Ok(history);
    }
    check_directory(&directory, true)?;
    let runs = if let Some(run) = run {
      vec![run]
    } else {
      let mut runs = std::collections::BTreeSet::new();
      for entry in fs::read_dir(directory)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".jsonl") {
          history.complete = false;
          history.warning = Some(
            "Legacy per-run JSONL logs remain in the history directory; they are not included in human-readable logs.",
          );
        }
        if let Some(id) = name
          .strip_suffix(".log")
          .and_then(|name| name.split('.').next())
          .and_then(|id| Uuid::parse_str(id).ok())
        {
          runs.insert(id);
        }
      }
      runs.into_iter().collect()
    };
    for run in runs {
      self.read_log_run(run, limit, failed_only, level, &mut history)?;
    }
    newest(&mut history.records, limit);
    history.records.reverse();
    Ok(history)
  }

  fn read_log_run(
    &self,
    run: Uuid,
    limit: usize,
    failed_only: bool,
    level: Option<Level>,
    history: &mut History,
  ) -> io::Result<()> {
    // Acquire the run lock before inspecting segments: rotation may briefly
    // remove the current filename. A selected run that never existed is empty.
    let lock_path = self.directory.join("logs").join(format!("{run}.lock"));
    if !exists(&lock_path)? {
      let mut has_segments = false;
      for index in 0..4 {
        has_segments |= exists(&self.log_path(run, index))?;
      }
      if !has_segments {
        return Ok(());
      }
    }
    let _lock = self.lock(run, false)?;
    for index in 0..4 {
      let path = self.log_path(run, index);
      if !exists(&path)? {
        continue;
      }
      let file = open(&path, false)?;
      if file.metadata()?.len() > self.segment_bytes {
        return Err(io::Error::other("history segment exceeds size limit"));
      }
      let mut bytes = Vec::new();
      file.take(self.segment_bytes + 1).read_to_end(&mut bytes)?;
      if bytes.len() as u64 > self.segment_bytes {
        return Err(io::Error::other("history segment exceeds size limit"));
      }
      // Visit newest lines first, preserving append order for equal
      // timestamps within a run when the stable sort merges process runs.
      let lines: Vec<_> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
      for line in lines.into_iter().rev() {
        let record = if line.len() <= MAX_RECORD_BYTES && line.ends_with(b"\n") {
          Record::from_log_line(line).filter(|record| record.valid() && record.run_id == run)
        } else {
          None
        };
        if let Some(record) = record {
          if (!failed_only || matches!(record.outcome, Outcome::Failed | Outcome::Interrupted))
            && level.is_none_or(|level| record.level >= level)
          {
            history.records.push(record);
          }
        } else {
          history.complete = false;
          history.warning = Some(
            "Some history records are malformed or use an unsupported schema; they were omitted.",
          );
        }
        // Keep memory bounded even when there are many process runs.
        if history.records.len() >= limit * 2 {
          newest(&mut history.records, limit);
        }
      }
    }
    newest(&mut history.records, limit);
    Ok(())
  }

  #[cfg(test)]
  pub(super) fn small(directory: PathBuf, segment_bytes: u64) -> Self {
    Self {
      directory,
      segment_bytes,
    }
  }

  fn lock(&self, run: Uuid, create: bool) -> io::Result<File> {
    let path = self.directory.join("logs").join(format!("{run}.lock"));
    let file = open(&path, create)?;
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
      match file.try_lock() {
        Ok(()) => return Ok(file),
        Err(TryLockError::Error(error)) => return Err(error),
        Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
          std::thread::sleep(Duration::from_millis(2));
        }
        Err(TryLockError::WouldBlock) => {
          return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "history store is busy",
          ));
        }
      }
    }
  }
}

fn newest(records: &mut Vec<Record>, limit: usize) {
  records.sort_by(|left, right| {
    right
      .timestamp_ms
      .cmp(&left.timestamp_ms)
      .then_with(|| right.run_id.cmp(&left.run_id))
  });
  records.truncate(limit);
}

fn exists(path: &Path) -> io::Result<bool> {
  match fs::symlink_metadata(path) {
    Ok(_) => Ok(true),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
    Err(error) => Err(error),
  }
}

fn prepare_directory(path: &Path) -> io::Result<()> {
  // Refuse symlink ancestors, and create directories with private permissions.
  if let Some(parent) = path.parent() {
    if exists(parent)? {
      check_directory(parent, false)?;
    } else {
      prepare_directory(parent)?;
    }
  }
  let mut builder = fs::DirBuilder::new();
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt as _;
    builder.mode(0o700);
  }
  match builder.create(path) {
    Ok(()) => {}
    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
    Err(error) => return Err(error),
  }
  check_directory(path, true)?;
  check_ancestors(path)
}

fn check_ancestors(path: &Path) -> io::Result<()> {
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    for ancestor in path.ancestors().skip(1) {
      let metadata = fs::symlink_metadata(ancestor)?;
      if metadata.uid() != rustix::process::getuid().as_raw() {
        break;
      }
      check_directory(ancestor, false)?;
    }
  }
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

fn check_directory(path: &Path, private: bool) -> io::Result<()> {
  let metadata = fs::symlink_metadata(path)?;
  if !metadata.is_dir() || metadata.file_type().is_symlink() {
    return Err(io::Error::other(
      "history directory must be a real directory",
    ));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.uid() != rustix::process::getuid().as_raw()
      || metadata.mode() & if private { 0o077 } else { 0o022 } != 0
    {
      return Err(io::Error::other(
        "history directory has unsafe ownership or permissions",
      ));
    }
  }
  #[cfg(not(unix))]
  let _ = private;
  Ok(())
}

fn open(path: &Path, create: bool) -> io::Result<File> {
  let mut options = OpenOptions::new();
  options.read(true);
  if create {
    options.append(true).create(true);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600).custom_flags(
      (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
        .bits()
        .cast_signed(),
    );
  }
  if exists(path)? && fs::symlink_metadata(path)?.file_type().is_symlink() {
    return Err(io::Error::other("history files must not be symbolic links"));
  }
  let file = options.open(path)?;
  let metadata = file.metadata()?;
  if !metadata.is_file() {
    return Err(io::Error::other("history files must be regular files"));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt as _;
    // SQLite can unlink a journal between open and fstat. A removed file is
    // absent, rather than an unsafe hardlink; callers decide whether absence is OK.
    if metadata.nlink() == 0 {
      return Err(io::Error::new(
        io::ErrorKind::NotFound,
        "history file was removed while opening",
      ));
    }
    if metadata.uid() != rustix::process::getuid().as_raw()
      || metadata.mode() & 0o077 != 0
      || metadata.nlink() != 1
    {
      return Err(io::Error::other(
        "history file has unsafe ownership, permissions, or links",
      ));
    }
  }
  Ok(file)
}
