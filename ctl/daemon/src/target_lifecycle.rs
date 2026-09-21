//! Orders explicit connect/disconnect requests without waiting for UI prompts.

use std::future::Future;

use tokio::sync::{Mutex, watch};

use super::RequestError;

#[derive(Clone, Copy, Default)]
struct Revision {
  generation: u64,
  paused: bool,
}

pub(super) struct TargetLifecycle {
  pub(super) lock: Mutex<()>,
  revision: watch::Sender<Revision>,
}

impl Default for TargetLifecycle {
  fn default() -> Self {
    Self {
      lock: Mutex::new(()),
      revision: watch::channel(Revision::default()).0,
    }
  }
}

impl TargetLifecycle {
  pub(super) fn attempt(&self) -> TargetAttempt {
    let changes = self.revision.subscribe();
    let generation = changes.borrow().generation;
    TargetAttempt {
      changes,
      generation,
    }
  }

  pub(super) fn pause(&self) {
    self.revision.send_modify(|revision| {
      revision.generation = revision.generation.wrapping_add(1);
      revision.paused = true;
    });
  }

  /// Only an explicit connect that arrived after the latest disconnect can
  /// resume this target. Queued older connects cannot undo the user's pause.
  pub(super) fn resume(&self, attempt: &TargetAttempt) -> Result<(), RequestError> {
    let mut current = false;
    self.revision.send_if_modified(|revision| {
      current = revision.generation == attempt.generation;
      if current && revision.paused {
        revision.paused = false;
        true
      } else {
        false
      }
    });
    if current {
      Ok(())
    } else {
      Err(RequestError::HostDisconnected)
    }
  }

  pub(super) fn is_paused(&self) -> bool {
    self.revision.borrow().paused
  }

  pub(super) fn require_connected(&self) -> Result<(), RequestError> {
    if self.is_paused() {
      Err(RequestError::HostDisconnected)
    } else {
      Ok(())
    }
  }
}

pub(super) struct TargetAttempt {
  changes: watch::Receiver<Revision>,
  generation: u64,
}

impl TargetAttempt {
  /// This also checks the generation before polling `work`: a completed old
  /// future must never take precedence over a pending disconnect notification.
  pub(super) async fn run<F: Future>(&mut self, work: F) -> Result<F::Output, RequestError> {
    tokio::select! {
      biased;
      _ = self.changes.wait_for(|revision| revision.generation != self.generation) => {
        Err(RequestError::HostDisconnected)
      }
      result = work => Ok(result),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;

  #[tokio::test]
  async fn disconnect_cancels_unanswered_prompts_and_queued_connects() {
    let lifecycle = Arc::new(TargetLifecycle::default());
    let mut active = lifecycle.attempt();
    let mut queued = lifecycle.attempt();
    let guard = lifecycle.lock.lock().await;
    let pending = tokio::spawn(async move { active.run(std::future::pending::<()>()).await });
    lifecycle.pause();
    assert!(matches!(
      pending.await.unwrap(),
      Err(RequestError::HostDisconnected)
    ));
    assert!(matches!(
      queued.run(lifecycle.lock.lock()).await,
      Err(RequestError::HostDisconnected)
    ));
    drop(guard);
    assert!(matches!(
      lifecycle.resume(&queued),
      Err(RequestError::HostDisconnected)
    ));
    assert!(lifecycle.is_paused());
  }

  #[tokio::test]
  async fn a_new_explicit_connect_resumes_without_reviving_stale_work() {
    let lifecycle = TargetLifecycle::default();
    let mut stale = lifecycle.attempt();
    lifecycle.pause();
    let mut fresh = lifecycle.attempt();
    let _guard = fresh.run(lifecycle.lock.lock()).await.unwrap();
    lifecycle.resume(&fresh).unwrap();
    assert!(!lifecycle.is_paused());
    assert!(matches!(
      stale.run(async { true }).await,
      Err(RequestError::HostDisconnected)
    ));
    assert!(fresh.run(async { true }).await.unwrap());
  }
}
