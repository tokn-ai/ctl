//! Credential approval state and lock/sleep revocation.
//!
//! The state machine is independent of the platform notification adapter, so its
//! fail-closed behavior can be tested without native session services.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[cfg(target_os = "macos")]
mod native;

#[cfg(target_os = "macos")]
pub use native::with_session_monitor;

type Revoke = Arc<dyn Fn() + Send + Sync>;
type Resume = Arc<dyn Fn(SessionResume) + Send + Sync>;

/// A full wake with an unlocked, active console following a monitored sleep.
///
/// Scheduling work does not authorize credential access. Consumers must check
/// `SessionState::resume_is_current` before acting on a queued event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionResume {
  pub generation: u64,
}

#[derive(Default)]
struct WakeState {
  pending: bool,
  screens_awake: bool,
}

/// A non-secret console-state signal owned by the daemon's main run loop.
pub struct SessionState {
  console_is_unlocked: fn() -> bool,
  available: AtomicBool,
  unlocked: AtomicBool,
  generation: AtomicU64,
  next_subscription: AtomicU64,
  subscribers: Mutex<BTreeMap<u64, Revoke>>,
  resume_subscribers: Mutex<BTreeMap<u64, Resume>>,
  wake: Mutex<WakeState>,
}

impl SessionState {
  fn new(console_is_unlocked: fn() -> bool) -> Self {
    Self {
      console_is_unlocked,
      available: AtomicBool::new(false),
      unlocked: AtomicBool::new(false),
      generation: AtomicU64::new(0),
      next_subscription: AtomicU64::new(0),
      subscribers: Mutex::new(BTreeMap::new()),
      resume_subscribers: Mutex::new(BTreeMap::new()),
      wake: Mutex::default(),
    }
  }

  /// Return false when registration or the current console state is unknown.
  /// Call this before and after using an authorization context, along with its
  /// generation. Reading state does not invoke subscribers reentrantly.
  #[must_use]
  pub fn is_unlocked(&self) -> bool {
    self.available.load(Ordering::Acquire)
      && self.unlocked.load(Ordering::Acquire)
      && (self.console_is_unlocked)()
  }

  /// Every delivered lock or sleep event changes the approval epoch, even if
  /// the console has already unlocked by the time its notification arrives.
  #[must_use]
  pub fn generation(&self) -> u64 {
    self.generation.load(Ordering::Acquire)
  }

  /// Recheck a queued resume event before beginning interactive recovery.
  /// A display-only sleep also stops recovery, without revoking approval for
  /// existing connections. This method never invokes subscription callbacks.
  #[must_use]
  pub fn resume_is_current(&self, event: SessionResume) -> bool {
    self.generation() == event.generation
      && self
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .screens_awake
      && self.is_unlocked()
  }

  /// Subscribe a fast revocation callback. It must not block on a connection,
  /// perform Keychain I/O, or panic: sleep waits for these callbacks to finish.
  pub fn subscribe(self: &Arc<Self>, revoke: Revoke) -> SessionSubscription {
    let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
    let mut subscribers = self
      .subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    subscribers.insert(id, revoke);
    SessionSubscription {
      state: Arc::downgrade(self),
      id,
      resume: false,
    }
  }

  /// Subscribe to one full-wake event per monitored sleep epoch. A dark wake,
  /// ordinary screen unlock, or initial monitor startup does not emit an event.
  /// When screens wake while locked, the event waits for the subsequent unlock.
  ///
  /// Callbacks run on macOS's main run loop. They must only enqueue work; never
  /// block, access Keychain, start connections, or display authentication UI.
  pub fn subscribe_resume(self: &Arc<Self>, resume: Resume) -> SessionSubscription {
    let id = self.next_subscription.fetch_add(1, Ordering::Relaxed);
    self
      .resume_subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id, resume);
    SessionSubscription {
      state: Arc::downgrade(self),
      id,
      resume: true,
    }
  }

  fn revoke(&self) {
    self.unlocked.store(false, Ordering::Release);
    self.generation.fetch_add(1, Ordering::AcqRel);
    let callbacks: Vec<_> = self
      .subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .values()
      .cloned()
      .collect();
    for revoke in callbacks {
      if catch_unwind(AssertUnwindSafe(|| revoke())).is_err() {
        self.available.store(false, Ordering::Release);
      }
    }
  }

  fn console_changed(&self, unlocked: bool) {
    // Secure-input changes also publish console security metadata. They must
    // not cancel the account-password dialog that authorizes a Keychain read.
    // The explicit distributed lock event remains unconditional; the kernel
    // notification is an additional early signal while the console is locked.
    if unlocked {
      self.console_available(unlocked);
    } else {
      self.revoke();
    }
  }

  fn will_sleep(&self) {
    *self
      .wake
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = WakeState {
      pending: true,
      screens_awake: false,
    };
    self.revoke();
  }

  fn console_available(&self, unlocked: bool) {
    self.unlocked.store(unlocked, Ordering::Release);
    self.resume_if_ready(unlocked);
  }

  fn powered_on(&self, unlocked: bool) {
    // Background maintenance wakes can leave the console unlocked. Only a
    // separate display-wake event permits scheduling interactive recovery.
    self.unlocked.store(unlocked, Ordering::Release);
  }

  fn screens_changed(&self, awake: bool, unlocked: bool) {
    self
      .wake
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .screens_awake = awake;
    self.console_available(unlocked);
  }

  fn resume_if_ready(&self, unlocked: bool) {
    if !unlocked || !self.available.load(Ordering::Acquire) {
      return;
    }
    {
      let mut wake = self
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if !wake.pending || !wake.screens_awake {
        return;
      }
      wake.pending = false;
    }
    let event = SessionResume {
      generation: self.generation(),
    };
    let callbacks: Vec<_> = self
      .resume_subscribers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .values()
      .cloned()
      .collect();
    let mut failed = false;
    for resume in callbacks {
      failed |= catch_unwind(AssertUnwindSafe(|| resume(event))).is_err();
    }
    if failed {
      self.available.store(false, Ordering::Release);
      self.revoke();
    }
  }
}

/// Retain this guard for as long as authorization contexts can be reused.
pub struct SessionSubscription {
  state: Weak<SessionState>,
  id: u64,
  resume: bool,
}

impl Drop for SessionSubscription {
  fn drop(&mut self) {
    if let Some(state) = self.state.upgrade() {
      if self.resume {
        state
          .resume_subscribers
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&self.id);
      } else {
        state
          .subscribers
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&self.id);
      }
    }
  }
}

#[cfg(test)]
mod tests;
