use super::{Outcome, Record, Stream};
use serde::Serialize;
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

  fn path(&self, stream: Stream, index: usize) -> PathBuf {
    let name = match stream {
      Stream::Logs => "logs",
      Stream::Audit => "audit",
    };
    self.directory.join(if index == 0 {
      format!("{name}.jsonl")
    } else {
      format!("{name}.{index}.jsonl")
    })
  }

  pub(super) fn append(&self, stream: Stream, record: &Record) -> io::Result<()> {
    if !record.valid() {
      return Err(io::Error::other("invalid history record"));
    }
    let mut encoded = serde_json::to_vec(record).map_err(io::Error::other)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_RECORD_BYTES {
      return Err(io::Error::other("history record exceeds size limit"));
    }
    prepare_directory(&self.directory)?;
    let _lock = self.lock(true)?;
    let path = self.path(stream, 0);
    let mut file = open(&path, true)?;
    let size = file.metadata()?.len();
    if size > self.segment_bytes {
      return Err(io::Error::other("history segment exceeds size limit"));
    }
    // Reserve one byte to separate a potentially interrupted final record.
    if size + encoded.len() as u64 + 1 > self.segment_bytes {
      drop(file);
      for index in (1..segments(stream)).rev() {
        let previous = self.path(stream, index - 1);
        let destination = self.path(stream, index);
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

  /// Read the newest matching records, returned in chronological file order.
  /// Missing history is empty; corruption is reported rather than hidden.
  ///
  /// # Errors
  /// Returns errors for unsafe paths, unavailable locks, oversized files, or I/O.
  pub fn read(&self, stream: Stream, limit: usize, failed_only: bool) -> io::Result<History> {
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
    let _lock = self.lock(false)?;
    for index in 0..segments(stream) {
      let path = self.path(stream, index);
      if !exists(&path)? {
        continue;
      }
      let file = open(&path, false)?;
      if file.metadata()?.len() > self.segment_bytes {
        return Err(io::Error::other("history segment exceeds size limit"));
      }
      let mut records = Vec::new();
      let mut bytes = Vec::new();
      file.take(self.segment_bytes + 1).read_to_end(&mut bytes)?;
      if bytes.len() as u64 > self.segment_bytes {
        return Err(io::Error::other("history segment exceeds size limit"));
      }
      for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let record = if line.len() <= MAX_RECORD_BYTES && line.ends_with(b"\n") {
          serde_json::from_slice::<Record>(line)
            .ok()
            .filter(Record::valid)
        } else {
          None
        };
        if let Some(record) = record {
          if !failed_only || matches!(record.outcome, Outcome::Failed | Outcome::Interrupted) {
            records.push(record);
          }
        } else {
          history.complete = false;
          history.warning = Some(
            "Some history records are malformed or use an unsupported schema; they were omitted.",
          );
        }
      }
      history.records.extend(
        records
          .into_iter()
          .rev()
          .take(limit - history.records.len()),
      );
      if history.records.len() == limit {
        break;
      }
    }
    history.records.reverse();
    Ok(history)
  }

  #[cfg(test)]
  pub(super) fn small(directory: PathBuf, segment_bytes: u64) -> Self {
    Self {
      directory,
      segment_bytes,
    }
  }

  fn lock(&self, create: bool) -> io::Result<File> {
    let path = self.directory.join("history.lock");
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

fn segments(stream: Stream) -> usize {
  match stream {
    Stream::Logs => 4,
    Stream::Audit => 16,
  }
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
