//! Reuse OS authorization, never passwords or decrypted private keys.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use ctl_core::observability::ConnectionContext;
use ctl_keychain_client::{
  Authentication, AuthenticationContext, SessionState, SessionSubscription,
};

use super::{Error, operation};
use crate::approval_context::{Grant, Shared};
use crate::reconnect_approval::{self, Windows};

pub(crate) mod selectors;
#[cfg(test)]
mod tests;
mod wake;

pub(crate) use crate::reconnect_approval::ReadDecision;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalReason {
  SessionChanged,
  CredentialChanged,
  RevisionUnavailable,
  Expired,
  ScopeChanged,
  Disconnect,
  HostConfirmation,
  CredentialReadFailed,
  Canceled,
}

pub(crate) type ApprovalObserver =
  Arc<dyn Fn(ApprovalReason, Option<&ConnectionContext>) + Send + Sync>;

impl reconnect_approval::Context for AuthenticationContext {
  fn invalidate(&self) {
    self.invalidate();
  }
  fn is_valid(&self) -> bool {
    !self.is_invalidated()
  }
}

pub(crate) struct Approvals {
  windows: Arc<Windows<Grant<AuthenticationContext>>>,
  native: Arc<Mutex<Vec<Weak<AuthenticationContext>>>>,
  known: Mutex<HashMap<String, (Instant, Vec<String>)>>,
  session: Arc<SessionState>,
  revision: Mutex<Option<String>>,
  observer: ApprovalObserver,
  _subscription: SessionSubscription,
}

impl Approvals {
  pub(crate) fn new(session: Arc<SessionState>, observer: ApprovalObserver) -> Arc<Self> {
    let windows = Arc::new(Windows::default());
    let native = Arc::new(Mutex::new(Vec::new()));
    let session_native = Arc::clone(&native);
    let weak = Arc::downgrade(&windows);
    let session_observer = Arc::clone(&observer);
    let subscription = session.subscribe(Arc::new(move || {
      let revoked = weak.upgrade().is_some_and(|windows| windows.revoke(None));
      invalidate_native(&session_native);
      if revoked {
        session_observer(ApprovalReason::SessionChanged, None);
      }
    }));
    Arc::new(Self {
      windows,
      native,
      known: Mutex::default(),
      session,
      revision: Mutex::new(None),
      observer,
      _subscription: subscription,
    })
  }

  fn check_revision(&self, revision: &str) {
    let mut previous = self.revision.lock().unwrap();
    if previous.as_deref() == Some(revision) {
      return;
    }
    // Publish the observed revision only after old authorization is revoked.
    // A concurrent begin must not match the new revision while old windows
    // are still available to inherit.
    let revoked = self.windows.revoke(None);
    invalidate_native(&self.native);
    self.known.lock().unwrap().clear();
    *previous = Some(revision.into());
    drop(previous);
    if revoked {
      (self.observer)(ApprovalReason::CredentialChanged, None);
    }
  }

  fn revoke(&self, reason: ApprovalReason) {
    let revoked = self.windows.revoke(None);
    invalidate_native(&self.native);
    if !matches!(reason, ApprovalReason::SessionChanged) {
      self.known.lock().unwrap().clear();
    }
    if revoked {
      (self.observer)(reason, None);
    }
  }

  pub(crate) fn revoke_target(&self, prefix: &str, connection: Option<&ConnectionContext>) {
    self
      .known
      .lock()
      .unwrap()
      .retain(|scope, _| !scope.starts_with(prefix));
    if self.windows.revoke_prefix(prefix) {
      (self.observer)(ApprovalReason::Disconnect, connection);
    }
  }

  fn expire(&self) {
    if self.windows.expire(Instant::now()) {
      (self.observer)(ApprovalReason::Expired, None);
    }
  }

  pub(crate) fn maintain(&self) {
    self.expire();
    if !self.session.is_unlocked() {
      self.revoke(ApprovalReason::SessionChanged);
    }
    match operation::revision() {
      Ok(revision) => self.check_revision(&revision),
      Err(_) => self.revoke(ApprovalReason::RevisionUnavailable),
    }
  }

  pub(crate) fn session(&self) -> &Arc<SessionState> {
    &self.session
  }

  pub(crate) fn selectors(&self, scope: &str) -> Vec<String> {
    self.expire();
    self
      .known
      .lock()
      .unwrap()
      .get(scope)
      .map_or_else(Vec::new, |(_, selectors)| selectors.clone())
  }

  fn remember(&self) {
    let approved = self.windows.approved_selectors();
    let mut known = self.known.lock().unwrap();
    for (scope, expires, selectors) in approved {
      if !selectors.is_empty() {
        known.insert(scope, (expires, selectors));
      }
    }
    // Keep the same bound as authorization windows even across sleep revocation.
    while known.len() > 128 {
      let oldest = known
        .iter()
        .min_by_key(|(_, (expires, _))| *expires)
        .map(|(scope, _)| scope.clone())
        .unwrap();
      known.remove(&oldest);
    }
  }

  fn context(&self) -> Result<Arc<AuthenticationContext>, Error> {
    let context = Arc::new(AuthenticationContext::new()?);
    let mut native = self.native.lock().unwrap();
    native.retain(|context| context.strong_count() > 0);
    native.push(Arc::downgrade(&context));
    Ok(context)
  }

  pub(crate) fn wake_batch(&self) -> Result<Arc<Shared<AuthenticationContext>>, Error> {
    let guard = operation::acquire()?;
    self.check_revision(&guard.revision()?);
    Ok(Shared::new(self.context()?))
  }
}

fn invalidate_native(native: &Mutex<Vec<Weak<AuthenticationContext>>>) {
  let mut native = native.lock().unwrap();
  native.retain(|context| {
    if let Some(context) = context.upgrade() {
      context.invalidate();
      true
    } else {
      false
    }
  });
}

/// Clones share one attempt, including late workers after an async cancellation.
#[derive(Clone)]
pub(crate) struct Attempt {
  approvals: Option<Arc<Approvals>>,
  pending: Option<reconnect_approval::Attempt<Grant<AuthenticationContext>>>,
  wake: Option<Arc<Shared<AuthenticationContext>>>,
  revision: Option<String>,
  session_generation: u64,
  history: Option<Arc<ConnectionContext>>,
  pub(crate) interactive: bool,
}

pub(crate) struct CancelOnDrop(Attempt);

impl Drop for CancelOnDrop {
  fn drop(&mut self) {
    if let Some(pending) = &self.0.pending
      && pending.cancel()
    {
      self.0.observe(ApprovalReason::Canceled);
    }
  }
}

impl Attempt {
  pub(crate) fn with_history(mut self, history: ConnectionContext) -> Self {
    self.history = Some(Arc::new(history));
    self
  }

  pub(crate) fn history(&self) -> Option<&ConnectionContext> {
    self.history.as_deref()
  }

  fn observe(&self, reason: ApprovalReason) {
    if let Some(approvals) = &self.approvals {
      (approvals.observer)(reason, self.history());
    }
  }

  pub(crate) fn reuse_failed(&self) -> bool {
    self
      .pending
      .as_ref()
      .is_some_and(reconnect_approval::Attempt::reuse_failed)
  }

  pub(crate) fn guard(&self) -> CancelOnDrop {
    CancelOnDrop(self.clone())
  }
  pub(crate) fn fresh(interactive: bool) -> Self {
    Self {
      approvals: None,
      pending: None,
      wake: None,
      revision: None,
      session_generation: 0,
      history: None,
      interactive,
    }
  }

  #[cfg(test)]
  pub(crate) fn failed_retained_fixture() -> Self {
    tests::failed_retained_attempt()
  }

  pub(crate) fn begin(
    approvals: Option<&Arc<Approvals>>,
    prefix: &str,
    scope: Option<String>,
    interactive: bool,
    history: ConnectionContext,
  ) -> Self {
    if let Some(approvals) = approvals
      && approvals.windows.select_scope(prefix, scope.as_deref())
    {
      (approvals.observer)(ApprovalReason::ScopeChanged, Some(&history));
    }
    let Some(approvals) = approvals.filter(|approvals| approvals.session.is_unlocked()) else {
      return Self::fresh(interactive).with_history(history);
    };
    let Some(scope) = scope else {
      return Self::fresh(interactive).with_history(history);
    };
    let Ok(guard) = operation::acquire() else {
      return Self::fresh(interactive).with_history(history);
    };
    let Ok(revision) = guard.revision() else {
      approvals.revoke(ApprovalReason::RevisionUnavailable);
      return Self::fresh(interactive).with_history(history);
    };
    approvals.check_revision(&revision);
    approvals.expire();
    let session_generation = approvals.session.generation();
    let pending = approvals.windows.begin(scope, Instant::now());
    Self {
      approvals: Some(Arc::clone(approvals)),
      pending: Some(pending),
      wake: None,
      revision: Some(revision),
      session_generation,
      history: Some(Arc::new(history)),
      interactive,
    }
  }

  pub(crate) fn for_wake(
    mut self,
    batch: Arc<Shared<AuthenticationContext>>,
  ) -> Result<Self, Error> {
    if self.pending.is_none() {
      return Err(unavailable());
    }
    self.wake = Some(batch);
    Ok(self)
  }

  fn valid(&self, guard: &operation::Guard) -> bool {
    let Some(approvals) = &self.approvals else {
      return false;
    };
    approvals.expire();
    let revision = guard.revision();
    if let Ok(revision) = &revision {
      approvals.check_revision(revision);
    }
    let revision_valid = revision.as_ref().ok() == self.revision.as_ref();
    let session_valid =
      approvals.session.is_unlocked() && approvals.session.generation() == self.session_generation;
    if !revision_valid {
      approvals.revoke(if revision.is_err() {
        ApprovalReason::RevisionUnavailable
      } else {
        ApprovalReason::CredentialChanged
      });
    }
    if !session_valid {
      approvals.revoke(ApprovalReason::SessionChanged);
    }
    revision_valid
      && session_valid
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
    read: impl FnOnce(Authentication<'_>, ReadDecision) -> Result<Option<T>, Error>,
  ) -> Result<Option<T>, Error> {
    let Some(pending) = &self.pending else {
      let decision = ReadDecision::fresh(self.interactive);
      let authentication = if decision.allow_ui() {
        Authentication::Allow { reason }
      } else {
        Authentication::Forbid
      };
      return read(authentication, decision);
    };
    pending
      .read(
        selector,
        self.interactive,
        || {
          if let Some(batch) = &self.wake {
            Ok(Grant::new(Arc::clone(&batch.native), false))
          } else {
            Ok(Grant::new(
              self.approvals.as_ref().ok_or_else(unavailable)?.context()?,
              true,
            ))
          }
        },
        || self.valid(guard),
        unavailable,
        |context, decision| {
          let decision = self.wake.as_ref().map_or(decision, |batch| {
            batch.decision_for(&context.native, decision)
          });
          let result = read(
            Authentication::Context {
              reason,
              context: &context.native,
              allow_ui: decision.allow_ui(),
            },
            decision,
          );
          if let Some(batch) = &self.wake {
            if matches!(&result, Ok(Some(_))) {
              batch.succeeded_for(&context.native);
            } else if !matches!(&result, Ok(Some(_))) {
              batch.native.invalidate();
            }
          }
          result
        },
      )
      .map_err(|failure| {
        if failure.revoked {
          self.observe(ApprovalReason::CredentialReadFailed);
        }
        failure.error
      })
  }

  pub(crate) fn connected(&self) -> bool {
    let Ok(guard) = operation::acquire() else {
      return false;
    };
    if self.valid(&guard)
      && let Some(pending) = &self.pending
    {
      pending.connected(Instant::now());
      if let Some(approvals) = &self.approvals {
        approvals.remember();
      }
      return pending.valid();
    }
    false
  }

  pub(crate) fn discard_cached(&self) {
    if let Some(pending) = &self.pending
      && pending.discard_cached()
    {
      self.observe(ApprovalReason::HostConfirmation);
    }
  }

  pub(crate) fn revoke(&self) {
    if let Some(pending) = &self.pending
      && pending.revoke()
    {
      self.observe(ApprovalReason::ScopeChanged);
    }
  }
}

fn unavailable() -> Error {
  Error::ApprovalUnavailable
}
