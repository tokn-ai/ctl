//! Orders native Connect/Stop requests before and during daemon dispatch.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ctld_ipc::VpnStatus;
use tokio::sync::{Mutex, Notify};

use crate::error::{CommandErrorDto, CommandResult};

pub(super) struct Coordinator {
  pub(super) changes: Mutex<()>,
  generation: AtomicU64,
  cancelled: Notify,
}

impl Coordinator {
  pub(super) const fn new() -> Self {
    Self {
      changes: Mutex::const_new(()),
      generation: AtomicU64::new(0),
      cancelled: Notify::const_new(),
    }
  }

  pub(super) async fn connect<'a, F, S>(&'a self, start: F) -> CommandResult<VpnStatus>
  where
    F: FnOnce(Cancellation<'a>) -> S,
    S: Future<Output = CommandResult<VpnStatus>>,
  {
    // Capture before the lock, so a Stop also cancels previously queued starts.
    let cancellation = Cancellation {
      owner: self,
      generation: self.generation.load(Ordering::SeqCst),
    };
    let _change = self.changes.lock().await;
    cancellation.check()?;
    start(cancellation).await
  }

  pub(super) async fn stop<F, S>(&self, stop: F) -> CommandResult<VpnStatus>
  where
    F: Fn() -> S,
    S: Future<Output = CommandResult<VpnStatus>>,
  {
    self.generation.fetch_add(1, Ordering::SeqCst);
    self.cancelled.notify_waiters();
    let ownership = self.changes.lock();
    tokio::pin!(ownership);
    let mut ticks = tokio::time::interval(Duration::from_millis(250));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
      tokio::select! {
        biased;
        _change = &mut ownership => return stop().await,
        _ = ticks.tick() => {
          // Keep the earlier Start future owned until it settles. It may still
          // be starting ctld, so an initial "stopped" response is not final.
          // Repeated Stop cancels startup once ctld observes that request.
          let _ = stop().await;
        }
      }
    }
  }
}

#[derive(Clone, Copy)]
pub(super) struct Cancellation<'a> {
  owner: &'a Coordinator,
  generation: u64,
}

impl Cancellation<'_> {
  pub(super) fn check(self) -> CommandResult<()> {
    if self.owner.generation.load(Ordering::SeqCst) != self.generation {
      return Err(cancelled());
    }
    Ok(())
  }

  pub(super) async fn wait(self) {
    loop {
      let notification = self.owner.cancelled.notified();
      tokio::pin!(notification);
      notification.as_mut().enable();
      if self.check().is_err() {
        return;
      }
      notification.await;
    }
  }
}

pub(super) fn cancelled() -> CommandErrorDto {
  CommandErrorDto::new("vpn_start_cancelled", "VPN connection was cancelled.")
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, AtomicUsize};

  use tokio::sync::oneshot;
  use tokio::time::timeout;

  use super::*;

  #[tokio::test]
  async fn stop_during_profile_load_prevents_any_later_start_dispatch() {
    let coordinator = Arc::new(Coordinator::new());
    let (loading, loaded) = oneshot::channel();
    let (release, profile) = oneshot::channel();
    let dispatched = Arc::new(AtomicBool::new(false));
    let started = {
      let coordinator = Arc::clone(&coordinator);
      let dispatched = Arc::clone(&dispatched);
      tokio::spawn(async move {
        coordinator
          .connect(|cancellation| async move {
            loading.send(()).unwrap();
            tokio::select! {
              () = cancellation.wait() => return Err(cancelled()),
              _ = profile => {},
            }
            cancellation.check()?;
            dispatched.store(true, Ordering::SeqCst);
            Ok(VpnStatus::default())
          })
          .await
      })
    };
    loaded.await.unwrap();
    timeout(
      Duration::from_secs(2),
      coordinator.stop(|| async { Ok(VpnStatus::default()) }),
    )
    .await
    .unwrap()
    .unwrap();
    let _ = release.send(());
    assert_eq!(
      started.await.unwrap().unwrap_err().code,
      "vpn_start_cancelled"
    );
    assert!(!dispatched.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn stop_waits_for_late_dispatch_and_confirms_the_start_is_stopped() {
    let coordinator = Arc::new(Coordinator::new());
    let (starting, began) = oneshot::channel();
    let (dispatch, ready_to_dispatch) = oneshot::channel();
    let active = Arc::new(AtomicBool::new(false));
    let stop_calls = Arc::new(AtomicUsize::new(0));
    let first_stop = Arc::new(Notify::new());
    let start_finished = Arc::new(Notify::new());
    let started = {
      let coordinator = Arc::clone(&coordinator);
      let active = Arc::clone(&active);
      let start_finished = Arc::clone(&start_finished);
      tokio::spawn(async move {
        coordinator
          .connect(|_| async move {
            starting.send(()).unwrap();
            ready_to_dispatch.await.unwrap();
            active.store(true, Ordering::SeqCst);
            start_finished.notified().await;
            Err(cancelled())
          })
          .await
      })
    };
    began.await.unwrap();
    let stopped = {
      let coordinator = Arc::clone(&coordinator);
      let active = Arc::clone(&active);
      let stop_calls = Arc::clone(&stop_calls);
      let first_stop = Arc::clone(&first_stop);
      let start_finished = Arc::clone(&start_finished);
      tokio::spawn(async move {
        coordinator
          .stop(|| {
            stop_calls.fetch_add(1, Ordering::SeqCst);
            first_stop.notify_one();
            if active.swap(false, Ordering::SeqCst) {
              start_finished.notify_one();
            }
            async { Ok(VpnStatus::default()) }
          })
          .await
      })
    };
    first_stop.notified().await;
    assert!(!stopped.is_finished());
    dispatch.send(()).unwrap();
    timeout(Duration::from_secs(2), stopped)
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(started.await.unwrap().is_err());
    assert!(!active.load(Ordering::SeqCst));
    assert!(stop_calls.load(Ordering::SeqCst) >= 3);
  }
}
