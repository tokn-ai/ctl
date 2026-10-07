//! Reuse OS authorization, never passwords or decrypted private keys.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use ctl_keychain_client::{
  Authentication, AuthenticationContext, SessionState, SessionSubscription,
};

use super::{Error, operation};
use crate::reconnect_approval::{self, Windows};

impl reconnect_approval::Context for AuthenticationContext {
  fn invalidate(&self) {
    self.invalidate();
  }
  fn is_valid(&self) -> bool {
    !self.is_invalidated()
  }
}

pub(crate) struct Approvals {
  windows: Arc<Windows<AuthenticationContext>>,
  session: Arc<SessionState>,
  revision: Mutex<Option<String>>,
  _subscription: SessionSubscription,
}

impl Approvals {
  pub(crate) fn new(session: Arc<SessionState>) -> Arc<Self> {
    let windows = Arc::new(Windows::default());
    let weak = Arc::downgrade(&windows);
    let subscription = session.subscribe(Arc::new(move || {
      if let Some(windows) = weak.upgrade() {
        windows.revoke(None);
      }
    }));
    Arc::new(Self {
      windows,
      session,
      revision: Mutex::new(None),
      _subscription: subscription,
    })
  }

  fn check_revision(&self, revision: &str) {
    let mut previous = self.revision.lock().unwrap();
    if previous.as_deref() != Some(revision) {
      self.windows.revoke(None);
      *previous = Some(revision.into());
    }
  }

  pub(crate) fn revoke(&self) {
    self.windows.revoke(None);
  }

  pub(crate) fn revoke_target(&self, prefix: &str) {
    self.windows.revoke_prefix(prefix);
  }

  pub(crate) fn maintain(&self) {
    self.windows.expire(Instant::now());
    if !self.session.is_unlocked() {
      self.revoke();
    }
    match operation::revision() {
      Ok(revision) => self.check_revision(&revision),
      Err(_) => self.revoke(),
    }
  }
}

/// Clones share one attempt, including late workers after an async cancellation.
#[derive(Clone)]
pub(crate) struct Attempt {
  approvals: Option<Arc<Approvals>>,
  pending: Option<reconnect_approval::Attempt<AuthenticationContext>>,
  revision: Option<String>,
  session_generation: u64,
  pub(crate) interactive: bool,
}

pub(crate) struct CancelOnDrop(Attempt);

impl Drop for CancelOnDrop {
  fn drop(&mut self) {
    if let Some(pending) = &self.0.pending {
      pending.cancel();
    }
  }
}

impl Attempt {
  pub(crate) fn guard(&self) -> CancelOnDrop {
    CancelOnDrop(self.clone())
  }
  pub(crate) fn fresh(interactive: bool) -> Self {
    Self {
      approvals: None,
      pending: None,
      revision: None,
      session_generation: 0,
      interactive,
    }
  }

  pub(crate) fn begin(
    approvals: Option<&Arc<Approvals>>,
    prefix: &str,
    scope: Option<String>,
    interactive: bool,
  ) -> Self {
    if let Some(approvals) = approvals {
      approvals.windows.select_scope(prefix, scope.as_deref());
    }
    let Some(approvals) = approvals.filter(|approvals| approvals.session.is_unlocked()) else {
      return Self::fresh(interactive);
    };
    let Some(scope) = scope else {
      return Self::fresh(interactive);
    };
    let Ok(guard) = operation::acquire() else {
      return Self::fresh(interactive);
    };
    let Ok(revision) = guard.revision() else {
      return Self::fresh(interactive);
    };
    approvals.check_revision(&revision);
    let session_generation = approvals.session.generation();
    let pending = approvals.windows.begin(scope, Instant::now());
    Self {
      approvals: Some(Arc::clone(approvals)),
      pending: Some(pending),
      revision: Some(revision),
      session_generation,
      interactive,
    }
  }

  fn valid(&self, guard: &operation::Guard) -> bool {
    let Some(approvals) = &self.approvals else {
      return false;
    };
    approvals.windows.expire(Instant::now());
    let revision = guard.revision();
    if let Ok(revision) = &revision {
      approvals.check_revision(revision);
    }
    let session_valid = revision.as_ref().ok() == self.revision.as_ref()
      && approvals.session.is_unlocked()
      && approvals.session.generation() == self.session_generation;
    if !session_valid {
      approvals.revoke();
    }
    session_valid
      && self
        .pending
        .as_ref()
        .is_some_and(reconnect_approval::Attempt::valid)
  }

  /// The caller holds the cross-process operation lock throughout this read.
  /// Native cancellation and the second check reject results racing revocation.
  pub(super) fn read<T>(
    &self,
    guard: &operation::Guard,
    selector: &str,
    reason: &str,
    read: impl FnOnce(Authentication<'_>) -> Result<Option<T>, Error>,
  ) -> Result<Option<T>, Error> {
    let Some(pending) = &self.pending else {
      let authentication = if self.interactive {
        Authentication::Allow { reason }
      } else {
        Authentication::Forbid
      };
      return read(authentication);
    };
    if !self.valid(guard) {
      return Err(unavailable());
    }
    let context = pending
      .context(selector, AuthenticationContext::new)?
      .ok_or_else(unavailable)?;
    if context.is_invalidated() {
      pending.failed(selector);
      return Err(unavailable());
    }
    let result = read(Authentication::Context {
      reason,
      context: &context,
      allow_ui: self.interactive,
    });
    if !self.valid(guard) || context.is_invalidated() {
      return Err(unavailable());
    }
    match result {
      Ok(Some(value)) => {
        if !pending.used(selector) {
          return Err(unavailable());
        }
        Ok(Some(value))
      }
      Ok(None) => Ok(None),
      Err(error) => {
        // An OS-invalidated or denied authorization is never retried through
        // a fresh context within the same attempt.
        pending.failed(selector);
        Err(error)
      }
    }
  }

  pub(crate) fn connected(&self) {
    let Ok(guard) = operation::acquire() else {
      return;
    };
    if self.valid(&guard)
      && let Some(pending) = &self.pending
    {
      pending.connected(Instant::now());
    }
  }

  pub(crate) fn discard_cached(&self) {
    if let Some(pending) = &self.pending {
      pending.discard_cached();
    }
  }

  pub(crate) fn revoke(&self) {
    if let Some(pending) = &self.pending {
      pending.revoke();
    }
  }
}

fn unavailable() -> Error {
  ctl_keychain_client::Error(-25_308).into()
}
