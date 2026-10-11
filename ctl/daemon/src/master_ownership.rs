//! Own a private master's startup until both SSH and authorization commit.
//!
//! `ssh -f` and `ControlPersist` fork a new process, so killing the child PID
//! cannot cancel a connected master. Pin its control connection and retain the
//! target lock until cleanup finishes; a queued Connect cannot replace it.

use std::io;
use std::sync::{Arc, Mutex};

use tokio::process::Child;
use tokio::sync::{OwnedMutexGuard, oneshot};

use super::MasterEndpoint;

#[derive(Clone, Default)]
pub(super) struct CleanupBarrier(Arc<Mutex<Option<oneshot::Receiver<io::Result<()>>>>>);

impl CleanupBarrier {
  pub(super) async fn wait(&self) -> io::Result<()> {
    let receiver = self.0.lock().unwrap().take();
    if let Some(receiver) = receiver {
      return receiver.await.unwrap_or(Ok(()));
    }
    Ok(())
  }
}

pub(super) struct StartupOwner<'a> {
  target_guard: OwnedMutexGuard<()>,
  barrier: &'a CleanupBarrier,
}

impl<'a> StartupOwner<'a> {
  pub(super) fn new(target_guard: OwnedMutexGuard<()>, barrier: &'a CleanupBarrier) -> Self {
    Self {
      target_guard,
      barrier,
    }
  }

  pub(super) fn attach(self, child: Child, endpoint: &MasterEndpoint) -> PendingMaster {
    PendingMaster::new(child, endpoint, self.target_guard, self.barrier)
  }
}

pub(super) struct PendingMaster {
  child: Option<Child>,
  target_guard: Option<OwnedMutexGuard<()>>,
  finished: Option<oneshot::Sender<io::Result<()>>>,
  shared: bool,
  #[cfg(unix)]
  control: private_control::Control,
}

impl PendingMaster {
  pub(super) fn new(
    child: Child,
    endpoint: &MasterEndpoint,
    target_guard: OwnedMutexGuard<()>,
    barrier: &CleanupBarrier,
  ) -> Self {
    let (finished, receiver) = oneshot::channel();
    *barrier.0.lock().unwrap() = Some(receiver);
    Self {
      child: Some(child),
      target_guard: Some(target_guard),
      finished: Some(finished),
      shared: endpoint.shared,
      #[cfg(unix)]
      control: private_control::Control::new(endpoint),
    }
  }

  pub(super) fn child(&mut self) -> &mut Child {
    self.child.as_mut().expect("pending SSH process")
  }

  #[cfg(unix)]
  pub(super) async fn pin(&mut self) -> io::Result<()> {
    if !self.shared {
      self.control.pin().await?;
    }
    Ok(())
  }

  pub(super) fn connected(mut self) -> OwnedMutexGuard<()> {
    let child = self.child.take().expect("pending SSH process");
    if self.shared {
      super::release_shared_process(child);
    } else {
      // Readiness can precede the -f parent exiting. Reap it without killing
      // a successfully authenticated process before its fork completes.
      tokio::spawn(async move {
        let mut child = child;
        let _ = child.wait().await;
      });
    }
    self.target_guard.take().expect("startup target lock")
  }
}

impl Drop for PendingMaster {
  fn drop(&mut self) {
    let Some(mut child) = self.child.take() else {
      return;
    };
    if self.shared {
      super::release_shared_process(child);
      return;
    }
    // Stop any further socket publication/forking immediately, before another
    // cancellation can drop the asynchronous cleanup task.
    let _ = child.start_kill();
    let target_guard = self.target_guard.take();
    let finished = self.finished.take();
    let cleanup = Cleanup {
      child,
      target_guard,
      finished,
      #[cfg(unix)]
      control: std::mem::take(&mut self.control),
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      runtime.spawn(async move {
        let mut cleanup = cleanup;
        let _ = cleanup.child.wait().await;
        // Native socket I/O is bounded and also available during runtime
        // teardown. The blocking job retains the lock even if its waiter dies.
        tokio::task::spawn_blocking(move || drop(cleanup));
      });
    }
    // If the runtime is already gone, Cleanup's Drop still uses the pinned
    // descriptor for bounded best-effort termination without a new runtime.
  }
}

struct Cleanup {
  child: Child,
  target_guard: Option<OwnedMutexGuard<()>>,
  finished: Option<oneshot::Sender<io::Result<()>>>,
  #[cfg(unix)]
  control: private_control::Control,
}

impl Drop for Cleanup {
  fn drop(&mut self) {
    #[cfg(unix)]
    let result = self.control.terminate();
    #[cfg(not(unix))]
    let result = Ok(());
    drop(self.target_guard.take());
    if let Some(finished) = self.finished.take() {
      let _ = finished.send(result);
    }
  }
}

#[cfg(unix)]
mod private_control;

#[cfg(all(test, unix))]
mod tests;
