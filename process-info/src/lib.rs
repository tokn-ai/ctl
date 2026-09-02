//! Read-only OS observations anchored to a known shell process.
//!
//! No shell commands, prompt parsing, argv, environment, or startup-file edits.
//! Calls are synchronous and may block in the OS (notably cwd on network
//! filesystems). Run them off UI, async-runtime, and PTY-ingestion threads.
//! Missing fields are expected: permissions, process exit, and process-table
//! races must never prevent a terminal from working.

mod inspect;
#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::io;
use std::path::PathBuf;

/// A PID plus an OS-specific birth token, not a portable timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
  pub pid: u32,
  pub start_time: u64,
}

/// Process identity and executable name only; never arguments or environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
  pub identity: ProcessIdentity,
  pub parent_pid: u32,
  pub process_group: u32,
  pub name: Option<String>,
}

/// A terminal foreground group is not evidence of shell prompt/editing state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Foreground {
  /// No reliable foreground observation (including inaccessible/exited jobs).
  Unknown,
  /// The terminal foreground group is the root shell's own process group.
  Shell,
  /// A foreground group member whose ancestry was verified against the shell.
  Child(ProcessInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
  pub shell: ProcessInfo,
  /// The root shell's physical cwd, not the foreground child's directory or
  /// the shell's logical `$PWD` spelling through symbolic links.
  pub cwd: Option<PathBuf>,
  pub foreground: Foreground,
}

/// Retains only the shell's birth identity, never caches live process details.
#[derive(Debug, Clone)]
pub struct Inspector {
  shell: ProcessIdentity,
}

impl Inspector {
  /// Capture the identity of the child while the caller still owns its handle.
  ///
  /// # Errors
  /// Returns an error when the PID is invalid, inaccessible, or has exited.
  pub fn new(shell_pid: u32) -> io::Result<Self> {
    let shell = platform().process(shell_pid)?.identity;
    Ok(Self { shell })
  }

  /// Query current cwd and foreground job. Pass the terminal's foreground
  /// process-group ID, e.g. from `tcgetpgrp`, not an arbitrary child PID.
  ///
  /// # Errors
  /// Returns an error if the original shell cannot be verified or its PID was
  /// reused. Cwd/job failures are independent and yield absent/unknown fields.
  pub fn inspect(&self, foreground_process_group: Option<u32>) -> io::Result<Snapshot> {
    inspect::inspect(&platform(), self.shell, foreground_process_group)
  }
}

trait Source {
  fn process(&self, pid: u32) -> io::Result<ProcessInfo>;
  fn cwd(&self, pid: u32) -> io::Result<PathBuf>;
  fn group_members(&self, group: u32) -> io::Result<Vec<u32>>;
}

#[cfg(target_os = "macos")]
fn platform() -> macos::MacOs {
  macos::MacOs
}

#[cfg(target_os = "linux")]
fn platform() -> linux::Linux {
  linux::Linux::default()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform() -> Unsupported {
  Unsupported
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
struct Unsupported;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl Source for Unsupported {
  fn process(&self, _: u32) -> io::Result<ProcessInfo> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
  }
  fn cwd(&self, _: u32) -> io::Result<PathBuf> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
  }
  fn group_members(&self, _: u32) -> io::Result<Vec<u32>> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
  }
}

fn valid_pid(pid: u32) -> io::Result<i32> {
  i32::try_from(pid)
    .ok()
    .filter(|pid| *pid > 0)
    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
}

fn process_name(bytes: &[u8]) -> Option<String> {
  let name = std::str::from_utf8(bytes).ok()?;
  (!name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control))
    .then(|| name.to_owned())
}
