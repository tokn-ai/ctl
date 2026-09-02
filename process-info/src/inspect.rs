use crate::{Foreground, ProcessIdentity, ProcessInfo, Snapshot, Source};
use std::collections::HashSet;
use std::io;

const MAX_ANCESTORS: usize = 64;
const MAX_GROUP_MEMBERS: usize = 256;

pub(super) fn inspect(
  source: &impl Source,
  identity: ProcessIdentity,
  group: Option<u32>,
) -> io::Result<Snapshot> {
  let shell = verified_shell(source, identity)?;
  let cwd = source
    .cwd(identity.pid)
    .ok()
    .filter(|path| path.is_absolute());
  let foreground = foreground(source, &shell, group);
  // A cwd syscall or tree walk may outlive the original process. Never attach
  // the resulting data to a later process that happens to reuse its PID.
  verified_shell(source, identity)?;
  Ok(Snapshot {
    shell,
    cwd,
    foreground,
  })
}

fn verified_shell(source: &impl Source, identity: ProcessIdentity) -> io::Result<ProcessInfo> {
  let shell = source.process(identity.pid)?;
  if shell.identity != identity {
    return Err(io::Error::new(
      io::ErrorKind::NotFound,
      "shell PID was reused",
    ));
  }
  Ok(shell)
}

fn foreground(source: &impl Source, shell: &ProcessInfo, group: Option<u32>) -> Foreground {
  let Some(group) = group.filter(|group| *group > 0) else {
    return Foreground::Unknown;
  };
  if group == shell.process_group {
    return Foreground::Shell;
  }
  // Prefer the job's group leader over transient workers it happens to spawn.
  if let Some(process) = candidate(source, shell, group, group) {
    return Foreground::Child(process);
  }
  // A pipeline's leader can exit while other group members still own the tty.
  let Ok(mut pids) = source.group_members(group) else {
    return Foreground::Unknown;
  };
  pids.sort_unstable();
  pids.dedup();
  for pid in pids.into_iter().take(MAX_GROUP_MEMBERS) {
    if let Some(process) = candidate(source, shell, group, pid) {
      return Foreground::Child(process);
    }
  }
  Foreground::Unknown
}

fn candidate(
  source: &impl Source,
  shell: &ProcessInfo,
  group: u32,
  pid: u32,
) -> Option<ProcessInfo> {
  let process = source.process(pid).ok()?;
  if process.process_group != group || process.identity.pid == shell.identity.pid {
    return None;
  }
  let mut ancestor = process.clone();
  let mut visited = HashSet::new();
  for _ in 0..MAX_ANCESTORS {
    if !visited.insert(ancestor.identity.pid) {
      return None;
    }
    if ancestor.identity == shell.identity {
      let current = source.process(pid).ok()?;
      return (current.identity == process.identity
        && current.parent_pid == process.parent_pid
        && current.process_group == group)
        .then_some(current);
    }
    let parent = source.process(ancestor.parent_pid).ok()?;
    // Reject a likely reused ancestor. Clock changes can conservatively make
    // macOS ancestry unavailable; never guess around an inconsistent ordering.
    if parent.identity.start_time > ancestor.identity.start_time {
      return None;
    }
    ancestor = parent;
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::RefCell;
  use std::collections::HashMap;
  use std::path::PathBuf;

  struct Fake {
    processes: RefCell<HashMap<u32, ProcessInfo>>,
    cwd: Option<PathBuf>,
    reuse_during_cwd: bool,
  }

  impl Source for Fake {
    fn process(&self, pid: u32) -> io::Result<ProcessInfo> {
      self
        .processes
        .borrow()
        .get(&pid)
        .cloned()
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn cwd(&self, pid: u32) -> io::Result<PathBuf> {
      if self.reuse_during_cwd {
        self
          .processes
          .borrow_mut()
          .get_mut(&pid)
          .unwrap()
          .identity
          .start_time += 1;
      }
      self
        .cwd
        .clone()
        .ok_or_else(|| io::Error::from(io::ErrorKind::PermissionDenied))
    }
    fn group_members(&self, group: u32) -> io::Result<Vec<u32>> {
      Ok(
        self
          .processes
          .borrow()
          .values()
          .filter(|process| process.process_group == group)
          .map(|process| process.identity.pid)
          .collect(),
      )
    }
  }

  fn process(pid: u32, parent_pid: u32, process_group: u32) -> ProcessInfo {
    ProcessInfo {
      identity: ProcessIdentity {
        pid,
        start_time: u64::from(pid),
      },
      parent_pid,
      process_group,
      name: Some(format!("process-{pid}")),
    }
  }

  fn source(processes: Vec<ProcessInfo>) -> Fake {
    Fake {
      processes: RefCell::new(
        processes
          .into_iter()
          .map(|process| (process.identity.pid, process))
          .collect(),
      ),
      cwd: Some("/shell-directory".into()),
      reuse_during_cwd: false,
    }
  }

  const ROOT: ProcessIdentity = ProcessIdentity {
    pid: 10,
    start_time: 10,
  };

  #[test]
  fn idle_shell_and_unknown_foreground_are_distinct() {
    let source = source(vec![process(10, 1, 10), process(20, 10, 20)]);
    assert_eq!(
      inspect(&source, ROOT, Some(10)).unwrap().foreground,
      Foreground::Shell
    );
    assert_eq!(
      inspect(&source, ROOT, None).unwrap().foreground,
      Foreground::Unknown
    );
    assert_eq!(
      inspect(&source, ROOT, Some(0)).unwrap().foreground,
      Foreground::Unknown
    );
  }

  #[test]
  fn prefers_job_leader_over_nested_workers_and_background_jobs() {
    let job = process(20, 10, 20);
    let source = source(vec![
      process(10, 1, 10),
      job.clone(),
      process(30, 20, 20),
      process(40, 10, 40),
    ]);
    let snapshot = inspect(&source, ROOT, Some(20)).unwrap();
    assert_eq!(snapshot.foreground, Foreground::Child(job));
    assert_eq!(snapshot.cwd, Some("/shell-directory".into()));
  }

  #[test]
  fn finds_a_job_under_nested_shells() {
    let job = process(30, 20, 30);
    let source = source(vec![process(10, 1, 10), process(20, 10, 20), job.clone()]);
    assert_eq!(
      inspect(&source, ROOT, Some(30)).unwrap().foreground,
      Foreground::Child(job)
    );
  }

  #[test]
  fn finds_surviving_pipeline_member_after_group_leader_exits() {
    let member = process(21, 10, 20);
    let source = source(vec![process(10, 1, 10), member.clone()]);
    assert_eq!(
      inspect(&source, ROOT, Some(20)).unwrap().foreground,
      Foreground::Child(member)
    );
  }

  #[test]
  fn refuses_unrelated_foreground_pid_and_cyclic_ancestry() {
    let source = source(vec![
      process(10, 1, 10),
      process(20, 30, 20),
      process(30, 20, 20),
    ]);
    assert_eq!(
      inspect(&source, ROOT, Some(20)).unwrap().foreground,
      Foreground::Unknown
    );
    source.processes.borrow_mut().insert(20, process(20, 1, 20));
    assert_eq!(
      inspect(&source, ROOT, Some(20)).unwrap().foreground,
      Foreground::Unknown
    );
  }

  #[test]
  fn cwd_failure_does_not_hide_a_known_job() {
    let job = process(20, 10, 20);
    let mut source = source(vec![process(10, 1, 10), job.clone()]);
    source.cwd = None;
    let snapshot = inspect(&source, ROOT, Some(20)).unwrap();
    assert_eq!(snapshot.cwd, None);
    assert_eq!(snapshot.foreground, Foreground::Child(job));
  }

  #[test]
  fn relative_cwd_is_not_an_operational_path() {
    let mut source = source(vec![process(10, 1, 10)]);
    source.cwd = Some("relative".into());
    assert_eq!(inspect(&source, ROOT, Some(10)).unwrap().cwd, None);
  }

  #[test]
  fn rejects_pid_reuse_before_and_during_observation() {
    let mut source = source(vec![process(10, 1, 10)]);
    let old = ProcessIdentity {
      start_time: 9,
      ..ROOT
    };
    assert_eq!(
      inspect(&source, old, None).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
    source.reuse_during_cwd = true;
    assert_eq!(
      inspect(&source, ROOT, None).unwrap_err().kind(),
      io::ErrorKind::NotFound
    );
  }

  #[test]
  fn missing_shell_cannot_adopt_an_unrelated_process() {
    let source = source(vec![process(20, 1, 20)]);
    assert!(inspect(&source, ROOT, Some(20)).is_err());
  }
}
