use crate::{ProcessIdentity, ProcessInfo, Source, process_name, valid_pid};
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

const MAX_STAT_BYTES: u64 = 16 * 1024;
const MAX_SCANNED_PROCESSES: usize = 65_536;

pub(super) struct Linux {
  proc: PathBuf,
}

impl Default for Linux {
  fn default() -> Self {
    Self {
      proc: PathBuf::from("/proc"),
    }
  }
}

impl Source for Linux {
  fn process(&self, pid: u32) -> io::Result<ProcessInfo> {
    valid_pid(pid)?;
    let mut bytes = Vec::new();
    fs::File::open(self.proc.join(pid.to_string()).join("stat"))?
      .take(MAX_STAT_BYTES + 1)
      .read_to_end(&mut bytes)?;
    if bytes.len() > usize::try_from(MAX_STAT_BYTES).unwrap_or(0) {
      return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    parse_stat(pid, &bytes)
  }

  fn cwd(&self, pid: u32) -> io::Result<PathBuf> {
    valid_pid(pid)?;
    let path = fs::read_link(self.proc.join(pid.to_string()).join("cwd"))?;
    // Linux decorates an unlinked cwd; this is not an operational path.
    if path.as_os_str().as_encoded_bytes().ends_with(b" (deleted)") {
      return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    Ok(path)
  }

  fn group_members(&self, group: u32) -> io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in fs::read_dir(&self.proc)?
      .take(MAX_SCANNED_PROCESSES)
      .flatten()
    {
      let Some(pid) = entry
        .file_name()
        .to_str()
        .and_then(|name| name.parse().ok())
      else {
        continue;
      };
      if self
        .process(pid)
        .is_ok_and(|process| process.process_group == group)
      {
        pids.push(pid);
      }
    }
    Ok(pids)
  }
}

fn parse_stat(pid: u32, bytes: &[u8]) -> io::Result<ProcessInfo> {
  let invalid = || io::Error::from(io::ErrorKind::InvalidData);
  // comm may contain spaces, parentheses, or newlines. The final ')' is the
  // delimiter; splitting the entire record on whitespace corrupts field offsets.
  let open = bytes
    .iter()
    .position(|byte| *byte == b'(')
    .ok_or_else(invalid)?;
  let close = bytes
    .iter()
    .rposition(|byte| *byte == b')')
    .ok_or_else(invalid)?;
  if close <= open {
    return Err(invalid());
  }
  let actual: u32 = std::str::from_utf8(&bytes[..open])
    .map_err(|_| invalid())?
    .trim()
    .parse()
    .map_err(|_| invalid())?;
  if actual != pid {
    return Err(invalid());
  }
  let rest = std::str::from_utf8(&bytes[close + 1..]).map_err(|_| invalid())?;
  let fields: Vec<_> = rest.split_whitespace().collect();
  let number = |index: usize| -> io::Result<u64> {
    fields
      .get(index)
      .ok_or_else(invalid)?
      .parse()
      .map_err(|_| invalid())
  };
  // Fields 3=state, 4=ppid, 5=pgrp, 22=starttime.
  if matches!(fields.first().copied(), Some("Z" | "X" | "x")) {
    return Err(io::Error::from(io::ErrorKind::NotFound));
  }
  Ok(ProcessInfo {
    identity: ProcessIdentity {
      pid,
      start_time: number(19)?,
    },
    parent_pid: u32::try_from(number(1)?).map_err(|_| invalid())?,
    process_group: u32::try_from(number(2)?).map_err(|_| invalid())?,
    name: process_name(&bytes[open + 1..close]),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn stat(name: &str, state: &str) -> Vec<u8> {
    format!("42 ({name}) {state} 10 42 10 0 42 0 0 0 0 0 0 0 0 0 0 0 0 0 1234\n").into_bytes()
  }

  #[test]
  fn parses_names_with_spaces_and_parentheses_without_shifting_fields() {
    let process = parse_stat(42, &stat("a (b) c)", "S")).unwrap();
    assert_eq!(
      process.identity,
      ProcessIdentity {
        pid: 42,
        start_time: 1234
      }
    );
    assert_eq!(process.parent_pid, 10);
    assert_eq!(process.process_group, 42);
    assert_eq!(process.name.as_deref(), Some("a (b) c)"));
  }

  #[test]
  fn omits_control_character_names_without_losing_identity() {
    assert_eq!(parse_stat(42, &stat("a\nb", "S")).unwrap().name, None);
  }

  #[test]
  fn rejects_truncated_wrong_pid_and_zombie_records() {
    assert!(parse_stat(42, b"42 (sh) S 10").is_err());
    assert!(parse_stat(43, &stat("sh", "S")).is_err());
    assert_eq!(
      parse_stat(42, &stat("sh", "Z")).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
  }

  #[cfg(not(target_os = "linux"))]
  #[test]
  fn absent_proc_mount_is_unavailable() {
    let source = Linux {
      proc: PathBuf::from("/nonexistent-process-info-proc"),
    };
    assert!(source.process(42).is_err());
    assert!(source.cwd(42).is_err());
    assert!(source.group_members(42).is_err());
  }
}
