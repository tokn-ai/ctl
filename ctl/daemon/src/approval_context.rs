//! Independent host grants can share one native wake authorization.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::reconnect_approval::{Context, ReadDecision};

pub(crate) struct Shared<C: Context> {
  pub(crate) native: Arc<C>,
  requested: AtomicBool,
  authorized: AtomicBool,
}

impl<C: Context> Shared<C> {
  pub(crate) fn new(native: Arc<C>) -> Arc<Self> {
    Arc::new(Self {
      native,
      requested: AtomicBool::new(false),
      authorized: AtomicBool::new(false),
    })
  }

  pub(crate) fn decision(&self) -> ReadDecision {
    if self.authorized.load(Ordering::Acquire) {
      ReadDecision::RetainedApproval
    } else if !self.requested.swap(true, Ordering::AcqRel) {
      ReadDecision::FreshInteractive
    } else {
      ReadDecision::FreshQuiet
    }
  }

  pub(crate) fn succeeded(&self) {
    self.authorized.store(true, Ordering::Release);
  }

  pub(crate) fn decision_for(&self, native: &Arc<C>, inherited: ReadDecision) -> ReadDecision {
    if Arc::ptr_eq(native, &self.native) {
      self.decision()
    } else {
      inherited
    }
  }

  pub(crate) fn succeeded_for(&self, native: &Arc<C>) {
    if Arc::ptr_eq(native, &self.native) {
      self.succeeded();
    }
  }
}

pub(crate) struct Grant<C: Context> {
  pub(crate) native: Arc<C>,
  owned: bool,
  valid: AtomicBool,
}

impl<C: Context> Grant<C> {
  pub(crate) fn new(native: Arc<C>, owned: bool) -> Self {
    Self {
      native,
      owned,
      valid: AtomicBool::new(true),
    }
  }
}

impl<C: Context> Context for Grant<C> {
  fn invalidate(&self) {
    self.valid.store(false, Ordering::Release);
    if self.owned {
      self.native.invalidate();
    }
  }

  fn is_valid(&self) -> bool {
    self.valid.load(Ordering::Acquire) && self.native.is_valid()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::reconnect_approval::Windows;
  use std::time::Instant;

  #[derive(Default)]
  struct Native(AtomicBool);

  impl Context for Native {
    fn invalidate(&self) {
      self.0.store(true, Ordering::Release);
    }
    fn is_valid(&self) -> bool {
      !self.0.load(Ordering::Acquire)
    }
  }

  #[test]
  fn batch_allows_one_ui_request_and_never_retries_a_cancellation() {
    let batch = Shared::new(Arc::new(Native::default()));
    assert_eq!(batch.decision(), ReadDecision::FreshInteractive);
    assert_eq!(batch.decision(), ReadDecision::FreshQuiet);
    batch.native.invalidate();
    assert!(!batch.native.is_valid());
    assert_eq!(batch.decision(), ReadDecision::FreshQuiet);
  }

  #[test]
  fn successful_batch_reads_reuse_authorization_without_ui() {
    let batch = Shared::new(Arc::new(Native::default()));
    assert!(batch.decision().allow_ui());
    batch.succeeded();
    for _ in 0..3 {
      assert_eq!(batch.decision(), ReadDecision::RetainedApproval);
      assert!(!batch.decision().allow_ui());
    }
  }

  #[test]
  fn inherited_host_approval_does_not_authenticate_the_batch_context() {
    let batch = Shared::new(Arc::new(Native::default()));
    let inherited = Arc::new(Native::default());
    assert_eq!(
      batch.decision_for(&inherited, ReadDecision::RetainedApproval),
      ReadDecision::RetainedApproval
    );
    batch.succeeded_for(&inherited);
    assert_eq!(
      batch.decision_for(&batch.native, ReadDecision::FreshInteractive),
      ReadDecision::FreshInteractive
    );
    batch.succeeded_for(&batch.native);
    assert_eq!(
      batch.decision_for(&batch.native, ReadDecision::FreshInteractive),
      ReadDecision::RetainedApproval
    );
  }

  #[test]
  fn host_revocation_does_not_revoke_other_hosts_in_same_batch() {
    let windows = Arc::new(Windows::default());
    let native = Arc::new(Native::default());
    let now = Instant::now();
    for scope in ["host-a", "host-b"] {
      let attempt = windows.begin(scope.into(), now);
      attempt
        .read(
          "key",
          false,
          || Ok::<_, ()>(Grant::new(Arc::clone(&native), false)),
          || true,
          || (),
          |_, _| Ok(Some(())),
        )
        .unwrap();
      attempt.connected(now);
    }
    assert!(windows.revoke(Some("host-a")));
    assert!(native.is_valid());
    let other = windows.begin("host-b".into(), now);
    other
      .read(
        "key",
        false,
        || panic!("retained grant"),
        || true,
        || (),
        |grant, decision| {
          assert!(grant.is_valid());
          assert_eq!(decision, ReadDecision::RetainedApproval);
          Ok(Some(()))
        },
      )
      .unwrap();
    native.invalidate();
    assert!(
      other
        .read(
          "key",
          false,
          || panic!("retained grant"),
          || true,
          || (),
          |_, _| Ok(Some(())),
        )
        .is_err()
    );
  }
}
