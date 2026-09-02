//! One bounded metadata worker per session manager. Never hold a session,
//! registry, or PTY lock across a process-inspection syscall. A stalled kernel
//! cwd query can delay metadata but cannot stall PTY traffic or daemon shutdown.

use crate::session::Session;
use process_info::Inspector;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

const INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct Watched {
  inspector: Inspector,
  session: Weak<Session>,
}

#[derive(Default)]
struct Registry {
  stopped: bool,
  watched: Vec<Watched>,
}

#[derive(Default)]
struct Shared {
  registry: Mutex<Registry>,
  wake: Condvar,
}

pub(crate) struct ProcessMonitor(Arc<Shared>);

impl ProcessMonitor {
  pub(crate) fn new() -> Option<Self> {
    let shared = Arc::new(Shared::default());
    let worker = Arc::clone(&shared);
    thread::Builder::new()
      .name("rmux-process-info".into())
      .spawn(move || run(&worker))
      .ok()?;
    Some(Self(shared))
  }

  pub(crate) fn register(&self, inspector: Inspector, session: &Arc<Session>) {
    let mut registry = self
      .0
      .registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.watched.push(Watched {
      inspector,
      session: Arc::downgrade(session),
    });
    self.0.wake.notify_one();
  }
}

impl Drop for ProcessMonitor {
  fn drop(&mut self) {
    let mut registry = self
      .0
      .registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.stopped = true;
    self.0.wake.notify_one();
    // Do not join: OS cwd lookups on unavailable network mounts can wedge.
    // Only this single worker is outstanding, and it owns no live session.
  }
}

fn run(shared: &Shared) {
  loop {
    let watched = {
      let mut registry = shared
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if registry.stopped {
        return;
      }
      registry.watched.retain(|watched| {
        watched
          .session
          .upgrade()
          .is_some_and(|session| session.process_observation_enabled())
      });
      registry.watched.clone()
    };
    for watched in watched {
      let Some(group) = watched.session.upgrade().and_then(|session| {
        session
          .process_observation_enabled()
          .then(|| session.foreground_process_group())
      }) else {
        continue;
      };
      let observation = watched.inspector.inspect(group).ok();
      if shared
        .registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stopped
      {
        return;
      }
      if let Some(session) = watched.session.upgrade() {
        // Recheck the terminal group after the slow work. A changed foreground
        // group invalidates job identity without suppressing the shell's cwd.
        let observation = observation.map(|mut observation| {
          if session.foreground_process_group() != group {
            observation.foreground = process_info::Foreground::Unknown;
          }
          observation
        });
        session.apply_process_observation(observation);
      }
    }
    let registry = shared
      .registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if registry.stopped {
      return;
    }
    let _ = shared.wake.wait_timeout(registry, INTERVAL);
  }
}
