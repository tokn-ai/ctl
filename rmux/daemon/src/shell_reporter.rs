//! Private, local ingestion for shell-awareness reports.
//!
//! A shell integration writes complete snapshots to a unique FIFO owned by
//! `rmuxd`. The FIFO deliberately is not a protocol endpoint: it is local to
//! one daemon process and its child shells. The daemon assigns sequence and
//! revision metadata after receiving a validated [`ShellReport`].

use nix::sys::stat::Mode as NixMode;
use nix::unistd::mkfifo;
#[cfg(test)]
use rmux_proto::MAX_RUNNING_COMMAND_BYTES;
use rmux_proto::{
  CommandLine, PromptPhase, ShellCapabilities, ShellDescriptor, ShellType, is_valid_running_command,
};
use rustix::fs::{CWD, Mode, OFlags, openat};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;
use uuid::Uuid;

const REPORT_MAGIC_V1: &str = "rmux-shell-v1";
const REPORT_MAGIC_V2: &str = "rmux-shell-v2";
const REPORT_FIELD_COUNT: usize = 9;
const MAX_FIELD_BYTES: usize = 4 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_COMMAND_LINE_BYTES: usize = 4 * 1024;
const READ_BUFFER_BYTES: usize = 4 * 1024;
/// A reporting shell is untrusted. Coalescing and rate-limiting here keeps a
/// report flood from contending with raw PTY ingestion for the terminal lock.
const REPORT_DELIVERY_INTERVAL: Duration = Duration::from_millis(50);

/// A validated, complete shell-awareness snapshot from the local shell.
///
/// This deliberately has no session revision, observed output sequence, TUI
/// hint, or visibility policy fields. Those are owned by `rmuxd`, not by the
/// reporting shell.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ShellReport {
  pub(crate) shell: ShellDescriptor,
  pub(crate) cwd: Option<String>,
  pub(crate) prompt_phase: PromptPhase,
  pub(crate) current_command_line: Option<CommandLine>,
  pub(crate) running_command: Option<String>,
}

impl fmt::Debug for ShellReport {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter
      .debug_struct("ShellReport")
      .field("shell", &self.shell)
      .field("cwd_reported", &self.cwd.is_some())
      .field("prompt_phase", &self.prompt_phase)
      .field(
        "current_command_line_reported",
        &self.current_command_line.is_some(),
      )
      .field("running_command_reported", &self.running_command.is_some())
      .finish()
  }
}

/// Errors while creating, stopping, or cleaning up a private shell reporter.
#[derive(Debug, Error)]
pub(crate) enum ShellReporterError {
  #[error("could not create shell report FIFO: {0}")]
  Create(#[source] io::Error),
  #[error("could not open shell report FIFO: {0}")]
  Open(#[source] io::Error),
  #[error("could not start shell report reader: {0}")]
  ReaderThread(#[source] io::Error),
  #[error("could not start shell report delivery thread: {0}")]
  DeliveryThread(#[source] io::Error),
  #[error("shell report reader thread panicked")]
  ReaderThreadPanicked,
  #[error("shell report delivery thread panicked")]
  DeliveryThreadPanicked,
  #[error("could not wake shell report reader: {0}")]
  Wake(#[source] io::Error),
  #[error("could not remove shell report FIFO: {0}")]
  Cleanup(#[source] io::Error),
}

/// A private FIFO that receives shell-awareness reports for one daemon.
///
/// `runtime_directory` must already have been prepared as an owner-only rmux
/// runtime directory. The FIFO is created with mode `0600`, and is removed by
/// [`Self::shutdown`] or [`Drop`].
pub(crate) struct ShellReporter {
  path: PathBuf,
  /// Keeping a read/write endpoint open prevents shell writers from blocking
  /// while opening the FIFO and prevents the reader from seeing a temporary
  /// EOF between reports. It is nonblocking so shutdown cannot stall on a
  /// full pipe.
  keepalive: Option<File>,
  shutdown: Arc<AtomicBool>,
  reader: Option<JoinHandle<()>>,
  dispatcher: Arc<ReportDispatcher>,
  delivery: Option<JoinHandle<()>>,
}

impl ShellReporter {
  /// Creates a private FIFO and starts delivering validated reports.
  ///
  /// The callback runs on a dedicated, rate-limited delivery thread. It
  /// receives only complete reports that satisfy the wire-format and Unicode
  /// cursor invariants.
  pub(crate) fn new<F>(runtime_directory: &Path, on_report: F) -> Result<Self, ShellReporterError>
  where
    F: FnMut(ShellReport) + Send + 'static,
  {
    let path = create_fifo(runtime_directory)?;
    let keepalive = match open_fifo(&path, OFlags::RDWR | OFlags::NONBLOCK) {
      Ok(file) => file,
      Err(error) => {
        let _ignored = remove_fifo(&path);
        return Err(ShellReporterError::Open(error));
      }
    };
    let reader = match open_fifo(&path, OFlags::RDONLY) {
      Ok(file) => file,
      Err(error) => {
        drop(keepalive);
        let _ignored = remove_fifo(&path);
        return Err(ShellReporterError::Open(error));
      }
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let dispatcher = Arc::new(ReportDispatcher::default());
    let delivery_dispatcher = Arc::clone(&dispatcher);
    let delivery_thread = match thread::Builder::new()
      .name("rmux-shell-report-delivery".into())
      .spawn(move || delivery_dispatcher.deliver(on_report))
    {
      Ok(thread) => thread,
      Err(error) => {
        drop(keepalive);
        let _ignored = remove_fifo(&path);
        return Err(ShellReporterError::DeliveryThread(error));
      }
    };

    let reader_shutdown = Arc::clone(&shutdown);
    let reader_dispatcher = Arc::clone(&dispatcher);
    let reader_thread = match thread::Builder::new()
      .name("rmux-shell-reporter".into())
      .spawn(move || {
        read_reports(reader, &reader_shutdown, move |report| {
          reader_dispatcher.submit(report);
        });
      }) {
      Ok(thread) => thread,
      Err(error) => {
        dispatcher.stop();
        let _ignored = delivery_thread.join();
        drop(keepalive);
        let _ignored = remove_fifo(&path);
        return Err(ShellReporterError::ReaderThread(error));
      }
    };

    Ok(Self {
      path,
      keepalive: Some(keepalive),
      shutdown,
      reader: Some(reader_thread),
      dispatcher,
      delivery: Some(delivery_thread),
    })
  }

  /// Returns the private FIFO path to place in a child shell's environment.
  #[must_use]
  pub(crate) fn path(&self) -> &Path {
    &self.path
  }

  /// Stops the reader thread and removes the FIFO.
  ///
  /// The wake byte is never treated as protocol input: the reader checks the
  /// shutdown flag immediately after every blocking read and exits first.
  pub(crate) fn shutdown(&mut self) -> Result<(), ShellReporterError> {
    self.shutdown.store(true, Ordering::Release);
    self.dispatcher.stop();

    let wake_result = self.wake_reader();
    // Closing the only daemon-owned writer is a second wakeup path if a
    // transient write error prevented the one-byte wake record from entering
    // the FIFO.
    self.keepalive.take();
    let reader_result = self.join_reader();
    let delivery_result = self.join_delivery();
    let cleanup_result = remove_fifo(&self.path).map_err(ShellReporterError::Cleanup);

    wake_result
      .and(reader_result)
      .and(delivery_result)
      .and(cleanup_result)
  }

  fn wake_reader(&mut self) -> Result<(), ShellReporterError> {
    let Some(keepalive) = self.keepalive.as_mut() else {
      return Ok(());
    };

    loop {
      match keepalive.write(&[0]) {
        Ok(1) => return Ok(()),
        Ok(_) => {
          return Err(ShellReporterError::Wake(io::Error::new(
            io::ErrorKind::WriteZero,
            "shell report wake byte was not written",
          )));
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
        // A full pipe means the reader has data available and will observe the
        // shutdown flag as soon as that blocking read returns.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(ShellReporterError::Wake(error)),
      }
    }
  }

  fn join_reader(&mut self) -> Result<(), ShellReporterError> {
    let Some(reader) = self.reader.take() else {
      return Ok(());
    };
    reader
      .join()
      .map_err(|_| ShellReporterError::ReaderThreadPanicked)
  }

  fn join_delivery(&mut self) -> Result<(), ShellReporterError> {
    let Some(delivery) = self.delivery.take() else {
      return Ok(());
    };
    delivery
      .join()
      .map_err(|_| ShellReporterError::DeliveryThreadPanicked)
  }
}

impl Drop for ShellReporter {
  fn drop(&mut self) {
    let _ignored = self.shutdown();
  }
}

fn create_fifo(runtime_directory: &Path) -> Result<PathBuf, ShellReporterError> {
  const CREATE_ATTEMPTS: usize = 8;

  for _ in 0..CREATE_ATTEMPTS {
    let path = runtime_directory.join(format!("rmux-shell-{}.fifo", Uuid::new_v4()));
    match mkfifo(&path, NixMode::S_IRUSR | NixMode::S_IWUSR) {
      Ok(()) => return Ok(path),
      Err(nix::errno::Errno::EEXIST) => {}
      Err(error) => return Err(ShellReporterError::Create(error.into())),
    }
  }

  Err(ShellReporterError::Create(io::Error::new(
    io::ErrorKind::AlreadyExists,
    "could not allocate a unique shell report FIFO name",
  )))
}

fn open_fifo(path: &Path, flags: OFlags) -> io::Result<File> {
  let descriptor = openat(CWD, path, flags | OFlags::CLOEXEC, Mode::empty())?;
  Ok(File::from(descriptor))
}

fn remove_fifo(path: &Path) -> io::Result<()> {
  match fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

fn read_reports<F>(mut reader: File, shutdown: &AtomicBool, mut on_report: F)
where
  F: FnMut(ShellReport),
{
  let mut parser = RecordParser::default();
  let mut buffer = [0_u8; READ_BUFFER_BYTES];

  loop {
    match reader.read(&mut buffer) {
      Ok(0) => return,
      Ok(bytes_read) => {
        // Never feed the shutdown wake byte into the record parser. A report
        // already buffered at shutdown is intentionally discarded because the
        // session is being torn down.
        if shutdown.load(Ordering::Acquire) {
          return;
        }
        parser.push(&buffer[..bytes_read], &mut on_report);
      }
      Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
      Err(_) => return,
    }
  }
}

/// A single-slot mailbox between the untrusted FIFO reader and the daemon's
/// terminal state. Writers replace stale editable-line snapshots instead of
/// building an unbounded backlog, and the delivery side touches session state
/// at a bounded rate.
#[derive(Default)]
struct ReportDispatcher {
  state: Mutex<ReportDispatcherState>,
  changed: Condvar,
}

#[derive(Default)]
struct ReportDispatcherState {
  pending_report: Option<ShellReport>,
  stopped: bool,
}

impl ReportDispatcher {
  fn submit(&self, report: ShellReport) {
    let mut state = lock(&self.state);
    if state.stopped {
      return;
    }
    state.pending_report = Some(report);
    self.changed.notify_one();
  }

  fn stop(&self) {
    let mut state = lock(&self.state);
    state.stopped = true;
    state.pending_report = None;
    self.changed.notify_all();
  }

  fn deliver<F>(&self, mut on_report: F)
  where
    F: FnMut(ShellReport),
  {
    let mut next_delivery = Instant::now();
    loop {
      let report = {
        let mut state = lock(&self.state);
        loop {
          if state.stopped {
            return;
          }
          if state.pending_report.is_none() {
            state = wait(&self.changed, state);
            continue;
          }

          let now = Instant::now();
          if now < next_delivery {
            state = wait_timeout(&self.changed, state, next_delivery.duration_since(now));
            continue;
          }
          break state
            .pending_report
            .take()
            .expect("pending shell report checked above");
        }
      };

      on_report(report);
      next_delivery = Instant::now() + REPORT_DELIVERY_INTERVAL;
    }
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait<'a, T>(condition: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
  condition
    .wait(guard)
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn wait_timeout<'a, T>(
  condition: &Condvar,
  guard: MutexGuard<'a, T>,
  timeout: Duration,
) -> MutexGuard<'a, T> {
  condition
    .wait_timeout(guard, timeout)
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .0
}

#[derive(Default)]
struct RecordParser {
  fields: Vec<Vec<u8>>,
  field: Vec<u8>,
  record_bytes: usize,
  discard_delimiters_remaining: Option<usize>,
}

impl RecordParser {
  fn push<F>(&mut self, bytes: &[u8], on_report: &mut F)
  where
    F: FnMut(ShellReport),
  {
    for &byte in bytes {
      if self.discard_delimiters_remaining.is_some() {
        self.discard_byte(byte);
        continue;
      }

      if byte == 0 {
        self.complete_field(on_report);
      } else if self.field.len() >= MAX_FIELD_BYTES || self.record_bytes >= MAX_RECORD_BYTES {
        self.discard_record();
      } else {
        self.field.push(byte);
        self.record_bytes += 1;
      }
    }
  }

  fn complete_field<F>(&mut self, on_report: &mut F)
  where
    F: FnMut(ShellReport),
  {
    self.fields.push(std::mem::take(&mut self.field));
    if self.fields.len() != REPORT_FIELD_COUNT {
      return;
    }

    let fields = std::mem::take(&mut self.fields);
    self.record_bytes = 0;
    if let Some(report) = parse_report(&fields) {
      on_report(report);
    }
  }

  fn discard_record(&mut self) {
    // The current field has not reached its delimiter. Discard it and every
    // remaining field so the next byte after this record can start fresh.
    let delimiters = REPORT_FIELD_COUNT.saturating_sub(self.fields.len());
    self.fields.clear();
    self.field.clear();
    self.record_bytes = 0;
    self.discard_delimiters_remaining = Some(delimiters);
  }

  fn discard_byte(&mut self, byte: u8) {
    if byte != 0 {
      return;
    }

    let Some(remaining) = self.discard_delimiters_remaining else {
      return;
    };
    if remaining <= 1 {
      self.discard_delimiters_remaining = None;
    } else {
      self.discard_delimiters_remaining = Some(remaining - 1);
    }
  }
}

fn parse_report(fields: &[Vec<u8>]) -> Option<ShellReport> {
  let [
    magic,
    shell_type,
    integration_version,
    capabilities,
    cwd,
    prompt_phase,
    active_text_present,
    active_text,
    cursor_scalar_offset,
  ] = fields
  else {
    return None;
  };

  let format = match std::str::from_utf8(magic).ok()? {
    REPORT_MAGIC_V1 => ShellReportFormat::V1,
    REPORT_MAGIC_V2 => ShellReportFormat::V2,
    _ => return None,
  };
  let shell_type = parse_shell_type(std::str::from_utf8(shell_type).ok()?)?;
  let integration_version =
    parse_optional_u16(std::str::from_utf8(integration_version).ok()?).ok()?;
  let capabilities = parse_capabilities(std::str::from_utf8(capabilities).ok()?)?;
  let cwd = parse_optional_text(std::str::from_utf8(cwd).ok()?);
  let prompt_phase = parse_prompt_phase(std::str::from_utf8(prompt_phase).ok()?)?;
  let active_text_present = std::str::from_utf8(active_text_present).ok()?;
  let active_text = std::str::from_utf8(active_text).ok()?;
  let cursor_scalar_offset = std::str::from_utf8(cursor_scalar_offset).ok()?;

  // Version 2 preserves the v1 nine-field record shape. Its final three
  // fields describe one phase-exclusive active value: an editable buffer at
  // a prompt, or a non-editable command summary while running. Version 1 is
  // parsed with its original command-line semantics so existing integrations
  // remain compatible.
  let (current_command_line, running_command) = match format {
    ShellReportFormat::V1 => (
      parse_command_line(active_text_present, active_text, cursor_scalar_offset)?.into_option(),
      None,
    ),
    ShellReportFormat::V2 => match prompt_phase {
      PromptPhase::AtPrompt | PromptPhase::Editing => (
        parse_command_line(active_text_present, active_text, cursor_scalar_offset)?.into_option(),
        None,
      ),
      PromptPhase::Running => (
        None,
        parse_running_command(active_text_present, active_text, cursor_scalar_offset)?
          .into_option(),
      ),
      PromptPhase::Unknown => {
        if parse_absent_active_text(active_text_present, active_text, cursor_scalar_offset) {
          (None, None)
        } else {
          return None;
        }
      }
    },
  };

  if cwd.is_some() && !capabilities.reports_cwd
    || current_command_line.is_some() && !capabilities.reports_command_line
    || current_command_line
      .as_ref()
      .is_some_and(|line| line.cursor_scalar_offset.is_some())
      && !capabilities.reports_cursor
    || matches!(format, ShellReportFormat::V2)
      && prompt_phase == PromptPhase::Running
      && active_text_present == "1"
      && !capabilities.reports_running_command
    || prompt_phase != PromptPhase::Unknown && !capabilities.reports_prompt_phase
  {
    return None;
  }

  Some(ShellReport {
    shell: ShellDescriptor {
      shell_type,
      integration_version,
      capabilities,
    },
    cwd,
    prompt_phase,
    current_command_line,
    running_command,
  })
}

#[derive(Clone, Copy)]
enum ShellReportFormat {
  V1,
  V2,
}

/// A valid active-text field, preserving whether it was deliberately absent.
enum ActiveText<T> {
  Absent,
  Present(T),
}

impl<T> ActiveText<T> {
  fn into_option(self) -> Option<T> {
    match self {
      Self::Absent => None,
      Self::Present(value) => Some(value),
    }
  }
}

fn parse_command_line(
  active_text_present: &str,
  active_text: &str,
  cursor_scalar_offset: &str,
) -> Option<ActiveText<CommandLine>> {
  match active_text_present {
    "0" => parse_absent_active_text(active_text_present, active_text, cursor_scalar_offset)
      .then_some(ActiveText::Absent),
    "1" => {
      if active_text.len() > MAX_COMMAND_LINE_BYTES {
        return None;
      }
      let cursor_scalar_offset = parse_optional_u32(cursor_scalar_offset).ok()?;
      let command_line = CommandLine {
        text: active_text.into(),
        cursor_scalar_offset,
      };
      command_line
        .has_valid_cursor()
        .then_some(ActiveText::Present(command_line))
    }
    _ => None,
  }
}

fn parse_running_command(
  active_text_present: &str,
  active_text: &str,
  cursor_scalar_offset: &str,
) -> Option<ActiveText<String>> {
  match active_text_present {
    "0" => parse_absent_active_text(active_text_present, active_text, cursor_scalar_offset)
      .then_some(ActiveText::Absent),
    "1" if cursor_scalar_offset.is_empty() => {
      // An overlong or control-containing command must not leave the prior
      // editable state visible until the next prompt. Keep the trustworthy
      // running phase, but omit an invalid title preview instead of retaining
      // or truncating it.
      Some(if is_valid_running_command(active_text) {
        ActiveText::Present(active_text.into())
      } else {
        ActiveText::Absent
      })
    }
    _ => None,
  }
}

fn parse_absent_active_text(
  active_text_present: &str,
  active_text: &str,
  cursor_scalar_offset: &str,
) -> bool {
  active_text_present == "0" && active_text.is_empty() && cursor_scalar_offset.is_empty()
}

fn parse_shell_type(value: &str) -> Option<ShellType> {
  match value {
    "bash" => Some(ShellType::Bash),
    "fish" => Some(ShellType::Fish),
    "pwsh" => Some(ShellType::Pwsh),
    "zsh" => Some(ShellType::Zsh),
    "cmd" => Some(ShellType::Cmd),
    "sh" => Some(ShellType::Sh),
    "unknown" => Some(ShellType::Unknown),
    _ => None,
  }
}

fn parse_capabilities(value: &str) -> Option<ShellCapabilities> {
  let mut capabilities = ShellCapabilities::default();
  if value.is_empty() {
    return Some(capabilities);
  }

  for capability in value.split(',') {
    let reported = match capability {
      "cwd" => &mut capabilities.reports_cwd,
      "command_line" => &mut capabilities.reports_command_line,
      "cursor" => &mut capabilities.reports_cursor,
      "prompt_phase" => &mut capabilities.reports_prompt_phase,
      "running_command" => &mut capabilities.reports_running_command,
      _ => return None,
    };
    if *reported {
      return None;
    }
    *reported = true;
  }
  Some(capabilities)
}

fn parse_prompt_phase(value: &str) -> Option<PromptPhase> {
  match value {
    "unknown" => Some(PromptPhase::Unknown),
    "at_prompt" => Some(PromptPhase::AtPrompt),
    "editing" => Some(PromptPhase::Editing),
    "running" => Some(PromptPhase::Running),
    _ => None,
  }
}

fn parse_optional_text(value: &str) -> Option<String> {
  (!value.is_empty()).then(|| value.into())
}

fn parse_optional_u16(value: &str) -> Result<Option<u16>, ()> {
  if value.is_empty() {
    Ok(None)
  } else {
    value.parse().map(Some).map_err(|_| ())
  }
}

fn parse_optional_u32(value: &str) -> Result<Option<u32>, ()> {
  if value.is_empty() {
    Ok(None)
  } else {
    value.parse().map(Some).map_err(|_| ())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::os::unix::fs::{FileTypeExt, PermissionsExt};
  use std::sync::{Arc, Barrier, mpsc};
  use std::thread;
  use std::time::Duration;

  #[test]
  fn parses_a_fragmented_record_from_the_fifo() {
    let runtime_directory = TestRuntimeDirectory::new();
    let (sender, receiver) = mpsc::channel();
    let mut reporter = ShellReporter::new(runtime_directory.path(), move |report| {
      let _ignored = sender.send(report);
    })
    .expect("shell reporter should start");

    let record = report_record(&[
      REPORT_MAGIC_V1,
      "zsh",
      "1",
      "cwd,command_line,cursor,prompt_phase",
      "/tmp/example",
      "editing",
      "1",
      "echo 日",
      "6",
    ]);
    let mut writer = fs::OpenOptions::new()
      .write(true)
      .open(reporter.path())
      .expect("private FIFO should accept a shell writer");
    for chunk in record.chunks(3) {
      writer
        .write_all(chunk)
        .expect("fragmented record should be written");
    }
    writer.flush().expect("fragmented record should flush");
    drop(writer);

    let report = receiver
      .recv_timeout(Duration::from_secs(1))
      .expect("complete fragmented record should be delivered");
    assert_eq!(report.shell.shell_type, ShellType::Zsh);
    assert_eq!(report.shell.integration_version, Some(1));
    assert_eq!(report.cwd.as_deref(), Some("/tmp/example"));
    assert_eq!(report.prompt_phase, PromptPhase::Editing);
    let command_line = report
      .current_command_line
      .expect("reported active line should be retained");
    assert_eq!(command_line.text, "echo 日");
    assert_eq!(command_line.cursor_scalar_offset, Some(6));

    reporter.shutdown().expect("shell reporter should stop");
  }

  #[test]
  fn rejects_malformed_records_without_delivery() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V1,
      "zsh",
      "1",
      "command_line,cursor",
      "",
      "unknown",
      "1",
      "日",
      "2",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    assert!(reports.is_empty());
  }

  #[test]
  fn discards_oversized_records_and_resynchronizes() {
    let mut parser = RecordParser::default();
    let oversized_line = "x".repeat(MAX_COMMAND_LINE_BYTES + 1);
    let oversized = report_record_owned(&[
      REPORT_MAGIC_V1.into(),
      "bash".into(),
      "1".into(),
      "command_line".into(),
      String::new(),
      "unknown".into(),
      "1".into(),
      oversized_line,
      String::new(),
    ]);
    let valid = report_record(&[
      REPORT_MAGIC_V1,
      "bash",
      "1",
      "cwd,prompt_phase",
      "/tmp",
      "at_prompt",
      "0",
      "",
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&oversized, &mut receive);
    parser.push(&valid, &mut receive);

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].cwd.as_deref(), Some("/tmp"));
  }

  #[test]
  fn accepts_an_empty_active_command_line() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V1,
      "zsh",
      "1",
      "command_line,cursor,prompt_phase",
      "",
      "editing",
      "1",
      "",
      "0",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    let command_line = reports
      .pop()
      .expect("empty active line is still a report")
      .current_command_line
      .expect("empty active line should be present");
    assert_eq!(command_line.text, "");
    assert_eq!(command_line.cursor_scalar_offset, Some(0));
  }

  #[test]
  fn parses_a_v2_running_command_from_the_phase_exclusive_active_text() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V2,
      "zsh",
      "2",
      "cwd,command_line,cursor,prompt_phase,running_command",
      "/workspace",
      "running",
      "1",
      "cargo test --workspace",
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    let report = reports.pop().expect("v2 report should be accepted");
    assert_eq!(report.shell.integration_version, Some(2));
    assert!(report.shell.capabilities.reports_running_command);
    assert_eq!(report.current_command_line, None);
    assert_eq!(
      report.running_command.as_deref(),
      Some("cargo test --workspace")
    );
  }

  #[test]
  fn v2_rejects_running_command_text_outside_running_phase() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V2,
      "zsh",
      "2",
      "cwd,prompt_phase,running_command",
      "/workspace",
      "at_prompt",
      "1",
      "cargo test",
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    assert!(reports.is_empty());
  }

  #[test]
  fn v2_rejects_a_running_summary_without_its_advertised_capability() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V2,
      "zsh",
      "2",
      "cwd,prompt_phase",
      "/workspace",
      "running",
      "1",
      "cargo test",
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    assert!(reports.is_empty());
  }

  #[test]
  fn v2_omits_invalid_running_command_text_without_dropping_the_running_phase() {
    let mut parser = RecordParser::default();
    let record = report_record(&[
      REPORT_MAGIC_V2,
      "zsh",
      "2",
      "prompt_phase,running_command",
      "",
      "running",
      "1",
      "cargo\ntest",
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    let report = reports.pop().expect("running report should be retained");
    assert_eq!(report.prompt_phase, PromptPhase::Running);
    assert_eq!(report.running_command, None);
  }

  #[test]
  fn v2_omits_an_overlong_running_command_without_dropping_the_running_phase() {
    let mut parser = RecordParser::default();
    let overlong_command = "x".repeat(MAX_RUNNING_COMMAND_BYTES + 1);
    let record = report_record(&[
      REPORT_MAGIC_V2,
      "zsh",
      "2",
      "prompt_phase,running_command",
      "",
      "running",
      "1",
      &overlong_command,
      "",
    ]);
    let mut reports = Vec::new();
    let mut receive = |report| reports.push(report);
    parser.push(&record, &mut receive);

    let report = reports.pop().expect("running report should be retained");
    assert_eq!(report.prompt_phase, PromptPhase::Running);
    assert_eq!(report.running_command, None);
  }

  #[test]
  fn dispatcher_coalesces_reports_while_delivery_is_rate_limited() {
    let dispatcher = Arc::new(ReportDispatcher::default());
    let callback_started = Arc::new(Barrier::new(2));
    let callback_barrier = Arc::clone(&callback_started);
    let (sender, receiver) = mpsc::channel();
    let delivery_dispatcher = Arc::clone(&dispatcher);
    let delivery = thread::spawn(move || {
      let mut first = true;
      delivery_dispatcher.deliver(move |report| {
        sender
          .send(report)
          .expect("test receiver should remain available");
        if first {
          first = false;
          callback_barrier.wait();
        }
      });
    });

    dispatcher.submit(report_with_cwd("/first"));
    assert_eq!(
      receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("first report should be delivered")
        .cwd
        .as_deref(),
      Some("/first")
    );
    dispatcher.submit(report_with_cwd("/stale"));
    dispatcher.submit(report_with_cwd("/latest"));
    callback_started.wait();

    assert_eq!(
      receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("latest coalesced report should be delivered")
        .cwd
        .as_deref(),
      Some("/latest")
    );

    dispatcher.stop();
    delivery.join().expect("delivery thread should stop");
  }

  #[test]
  fn removes_private_fifo_on_drop() {
    let runtime_directory = TestRuntimeDirectory::new();
    let path = {
      let reporter =
        ShellReporter::new(runtime_directory.path(), |_| {}).expect("shell reporter should start");
      let path = reporter.path().to_path_buf();
      let metadata = fs::symlink_metadata(&path).expect("FIFO should exist");
      assert!(metadata.file_type().is_fifo());
      assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
      path
    };

    assert!(!path.exists());
  }

  fn report_record(fields: &[&str; REPORT_FIELD_COUNT]) -> Vec<u8> {
    let fields = fields.map(String::from);
    report_record_owned(&fields)
  }

  fn report_record_owned(fields: &[String; REPORT_FIELD_COUNT]) -> Vec<u8> {
    let mut record = Vec::new();
    for field in fields {
      record.extend_from_slice(field.as_bytes());
      record.push(0);
    }
    record
  }

  fn report_with_cwd(cwd: &str) -> ShellReport {
    ShellReport {
      shell: ShellDescriptor {
        shell_type: ShellType::Zsh,
        integration_version: Some(1),
        capabilities: ShellCapabilities {
          reports_cwd: true,
          reports_command_line: false,
          reports_cursor: false,
          reports_prompt_phase: true,
          reports_running_command: false,
        },
      },
      cwd: Some(cwd.into()),
      prompt_phase: PromptPhase::AtPrompt,
      current_command_line: None,
      running_command: None,
    }
  }

  struct TestRuntimeDirectory {
    path: PathBuf,
  }

  impl TestRuntimeDirectory {
    fn new() -> Self {
      let path = std::env::temp_dir().join(format!("rmux-shell-reporter-test-{}", Uuid::new_v4()));
      fs::create_dir(&path).expect("test runtime directory should be created");
      fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
        .expect("test runtime directory should be private");
      Self { path }
    }

    fn path(&self) -> &Path {
      &self.path
    }
  }

  impl Drop for TestRuntimeDirectory {
    fn drop(&mut self) {
      let _ignored = fs::remove_dir_all(&self.path);
    }
  }
}
