//! Bounded authorization windows. This module never holds credential values.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const WINDOW: Duration = Duration::from_hours(24);
const MAX_HOSTS: usize = 128;
const MAX_CONTEXTS: usize = 64;

pub(crate) trait Context: Send + Sync {
  fn invalidate(&self);
  fn is_valid(&self) -> bool;
}

/// A read's authorization policy, without credential or native context data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadDecision {
  FreshInteractive,
  FreshQuiet,
  RetainedApproval,
}

impl ReadDecision {
  pub(crate) fn allow_ui(self) -> bool {
    self == Self::FreshInteractive
  }

  pub(crate) fn fresh(interactive: bool) -> Self {
    if interactive {
      Self::FreshInteractive
    } else {
      Self::FreshQuiet
    }
  }
}

struct SelectedContext<C> {
  context: Arc<C>,
  retained_approval: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReadError<E> {
  pub(crate) error: E,
  pub(crate) revoked: bool,
}

impl<E> ReadError<E> {
  fn unchanged(error: E) -> Self {
    Self {
      error,
      revoked: false,
    }
  }
}

struct Window<C> {
  expires: Instant,
  contexts: HashMap<String, Arc<C>>,
}

struct Pending<C> {
  scope: String,
  valid: bool,
  expires: Option<Instant>,
  contexts: HashMap<String, Arc<C>>,
  inherited: HashSet<String>,
  used: HashSet<String>,
  reading: HashSet<String>,
  reuse_failed: bool,
  committed: bool,
}

struct Registry<C> {
  next: u64,
  windows: HashMap<String, Window<C>>,
  pending: HashMap<u64, Pending<C>>,
}

impl<C> Default for Registry<C> {
  fn default() -> Self {
    Self {
      next: 0,
      windows: HashMap::new(),
      pending: HashMap::new(),
    }
  }
}

pub(crate) struct Windows<C: Context> {
  registry: Mutex<Registry<C>>,
}

impl<C: Context> Default for Windows<C> {
  fn default() -> Self {
    Self {
      registry: Mutex::default(),
    }
  }
}

struct Handle<C: Context> {
  owner: Arc<Windows<C>>,
  id: u64,
}

pub(crate) struct Attempt<C: Context>(Arc<Handle<C>>);

impl<C: Context> Clone for Attempt<C> {
  fn clone(&self) -> Self {
    Self(Arc::clone(&self.0))
  }
}

impl<C: Context> Windows<C> {
  /// Nonsecret selectors from successful connections, for explicit wake consent.
  pub(crate) fn approved_selectors(&self) -> Vec<(String, Instant, Vec<String>)> {
    self
      .registry
      .lock()
      .unwrap()
      .windows
      .iter()
      .map(|(scope, window)| {
        (
          scope.clone(),
          window.expires,
          window
            .contexts
            .iter()
            .filter(|(_, context)| context.is_valid())
            .map(|(selector, _)| selector.clone())
            .collect(),
        )
      })
      .collect()
  }

  pub(crate) fn begin(self: &Arc<Self>, scope: String, now: Instant) -> Attempt<C> {
    self.expire(now);
    let mut registry = self.registry.lock().unwrap();
    let (expires, contexts) = registry.windows.get(&scope).map_or_else(
      || (None, HashMap::new()),
      |window| (Some(window.expires), window.contexts.clone()),
    );
    let inherited = contexts.keys().cloned().collect();
    let id = registry.next;
    registry.next = registry.next.checked_add(1).expect("bounded attempt IDs");
    registry.pending.insert(
      id,
      Pending {
        scope,
        valid: true,
        expires,
        contexts,
        inherited,
        used: HashSet::new(),
        reading: HashSet::new(),
        reuse_failed: false,
        committed: false,
      },
    );
    Attempt(Arc::new(Handle {
      owner: Arc::clone(self),
      id,
    }))
  }

  pub(crate) fn revoke(&self, scope: Option<&str>) -> bool {
    self.revoke_matching(|name| scope.is_none_or(|scope| scope == name))
  }

  pub(crate) fn revoke_prefix(&self, prefix: &str) -> bool {
    self.revoke_matching(|name| name.starts_with(prefix))
  }

  pub(crate) fn select_scope(&self, prefix: &str, current: Option<&str>) -> bool {
    self.revoke_matching(|name| name.starts_with(prefix) && Some(name) != current)
  }

  fn revoke_matching(&self, matches: impl Fn(&str) -> bool) -> bool {
    let mut registry = self.registry.lock().unwrap();
    let mut revoked = false;
    registry.windows.retain(|name, window| {
      if matches(name) {
        revoked |= window.contexts.values().any(|context| context.is_valid());
        invalidate(window.contexts.values());
        false
      } else {
        true
      }
    });
    for pending in registry.pending.values_mut() {
      if matches(&pending.scope) {
        revoked |= pending.valid && pending.contexts.values().any(|context| context.is_valid());
        pending.valid = false;
        invalidate(pending.contexts.values());
      }
    }
    revoked
  }

  pub(crate) fn expire(&self, now: Instant) -> bool {
    let mut registry = self.registry.lock().unwrap();
    let mut revoked = false;
    registry.windows.retain(|_, window| {
      window.contexts.retain(|_, context| context.is_valid());
      if window.contexts.is_empty() {
        return false;
      }
      if now >= window.expires {
        revoked = true;
        invalidate(window.contexts.values());
        false
      } else {
        true
      }
    });
    for pending in registry.pending.values_mut() {
      if pending.expires.is_some_and(|expires| now >= expires) {
        revoked |= pending.valid && pending.contexts.values().any(|context| context.is_valid());
        pending.valid = false;
        invalidate(pending.contexts.values());
      }
    }
    revoked
  }
}

fn invalidate<'a, C: Context + 'a>(contexts: impl Iterator<Item = &'a Arc<C>>) {
  for context in contexts {
    context.invalidate();
  }
}

impl<C: Context> Attempt<C> {
  /// Register a context before native authentication so revocation also cancels
  /// an in-flight query. The factory must not authenticate or retrieve data.
  fn context<E>(
    &self,
    selector: &str,
    create: impl FnOnce() -> Result<C, E>,
  ) -> Result<Option<SelectedContext<C>>, E> {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = registry.pending.get_mut(&self.0.id).expect("live attempt");
    if !pending.valid {
      return Ok(None);
    }
    if let Some(context) = pending.contexts.get(selector) {
      pending.reading.insert(selector.into());
      return Ok(Some(SelectedContext {
        context: Arc::clone(context),
        retained_approval: pending.inherited.contains(selector),
      }));
    }
    if pending.contexts.len() >= MAX_CONTEXTS {
      return Ok(None);
    }
    let context = Arc::new(create()?);
    pending
      .contexts
      .insert(selector.into(), Arc::clone(&context));
    pending.reading.insert(selector.into());
    Ok(Some(SelectedContext {
      context,
      retained_approval: false,
    }))
  }

  /// Reuse a committed approval without UI, including interactive reconnects.
  /// A denied native read revokes the attempt and never tries a fresh context.
  pub(crate) fn read<T, E>(
    &self,
    selector: &str,
    interactive: bool,
    create: impl FnOnce() -> Result<C, E>,
    valid: impl Fn() -> bool,
    unavailable: impl Fn() -> E,
    read: impl FnOnce(&C, ReadDecision) -> Result<Option<T>, E>,
  ) -> Result<Option<T>, ReadError<E>> {
    if !valid() {
      let inherited = self.0.owner.registry.lock().unwrap().pending[&self.0.id]
        .inherited
        .contains(selector);
      self.mark_reuse_failed(inherited);
      return Err(ReadError::unchanged(unavailable()));
    }
    let selected = self
      .context(selector, create)
      .map_err(ReadError::unchanged)?
      .ok_or_else(|| ReadError::unchanged(unavailable()))?;
    if !selected.context.is_valid() {
      self.mark_reuse_failed(selected.retained_approval);
      return Err(ReadError {
        error: unavailable(),
        revoked: self.failed(selector),
      });
    }
    let decision = if selected.retained_approval {
      ReadDecision::RetainedApproval
    } else {
      ReadDecision::fresh(interactive)
    };
    let result = read(&selected.context, decision);
    if !valid() {
      self.mark_reuse_failed(selected.retained_approval);
      return Err(ReadError::unchanged(unavailable()));
    }
    if !selected.context.is_valid() {
      self.mark_reuse_failed(selected.retained_approval);
      return Err(ReadError {
        error: unavailable(),
        revoked: self.failed(selector),
      });
    }
    match result {
      Ok(Some(value)) => {
        if !self.used(selector) {
          self.mark_reuse_failed(selected.retained_approval);
          return Err(ReadError::unchanged(unavailable()));
        }
        Ok(Some(value))
      }
      Ok(None) if !selected.retained_approval => Ok(None),
      Ok(None) => {
        self.mark_reuse_failed(true);
        Err(ReadError {
          error: unavailable(),
          revoked: self.failed(selector),
        })
      }
      Err(error) => {
        self.mark_reuse_failed(selected.retained_approval);
        Err(ReadError {
          error,
          revoked: self.failed(selector),
        })
      }
    }
  }

  fn mark_reuse_failed(&self, retained_approval: bool) {
    if retained_approval {
      self
        .0
        .owner
        .registry
        .lock()
        .unwrap()
        .pending
        .get_mut(&self.0.id)
        .expect("live attempt")
        .reuse_failed = true;
    }
  }

  pub(crate) fn reuse_failed(&self) -> bool {
    self.0.owner.registry.lock().unwrap().pending[&self.0.id].reuse_failed
  }

  pub(crate) fn used(&self, selector: &str) -> bool {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = registry.pending.get_mut(&self.0.id).expect("live attempt");
    pending.reading.remove(selector);
    if !pending.valid || !pending.contexts.contains_key(selector) {
      return false;
    }
    pending.used.insert(selector.into());
    true
  }

  pub(crate) fn valid(&self) -> bool {
    self.0.owner.registry.lock().unwrap().pending[&self.0.id].valid
  }

  pub(crate) fn failed(&self, selector: &str) -> bool {
    self
      .0
      .owner
      .registry
      .lock()
      .unwrap()
      .pending
      .get_mut(&self.0.id)
      .expect("live attempt")
      .reading
      .remove(selector);
    self.revoke()
  }

  /// A host confirmation requires fresh native authorization. Clear reusable
  /// contexts atomically, without reviving a canceled connection attempt.
  pub(crate) fn discard_cached(&self) -> bool {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = &registry.pending[&self.0.id];
    if !pending.valid {
      return false;
    }
    let scope = pending.scope.clone();
    let mut revoked = false;
    if let Some(window) = registry.windows.remove(&scope) {
      revoked |= window.contexts.values().any(|context| context.is_valid());
      invalidate(window.contexts.values());
    }
    for pending in registry.pending.values_mut() {
      if pending.scope == scope {
        revoked |= pending.valid && pending.contexts.values().any(|context| context.is_valid());
        invalidate(pending.contexts.values());
        pending.valid = false;
      }
    }
    let current = registry.pending.get_mut(&self.0.id).expect("live attempt");
    current.contexts.clear();
    current.inherited.clear();
    current.used.clear();
    current.reading.clear();
    current.reuse_failed = false;
    current.expires = None;
    current.committed = false;
    current.valid = true;
    revoked
  }

  pub(crate) fn revoke(&self) -> bool {
    let scope = self.0.owner.registry.lock().unwrap().pending[&self.0.id]
      .scope
      .clone();
    self.0.owner.revoke(Some(&scope))
  }

  pub(crate) fn cancel(&self) -> bool {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = &registry.pending[&self.0.id];
    if pending.committed {
      return false;
    }
    let mut revoked = false;
    for (name, context) in &pending.contexts {
      let retained = registry.windows.get(&pending.scope).is_some_and(|window| {
        window
          .contexts
          .get(name)
          .is_some_and(|saved| Arc::ptr_eq(saved, context))
      });
      if pending.reading.contains(name) || !retained {
        revoked |= pending.valid && context.is_valid();
        context.invalidate();
      }
    }
    registry
      .pending
      .get_mut(&self.0.id)
      .expect("live attempt")
      .valid = false;
    revoked
  }

  /// SSH success or explicit wake consent commits permission. Approved windows retain
  /// their original deadline; neither reads nor successful reconnects renew it.
  pub(crate) fn connected(&self, now: Instant) {
    self.0.owner.expire(now);
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = &registry.pending[&self.0.id];
    if !pending.valid || pending.used.is_empty() {
      return;
    }
    let scope = pending.scope.clone();
    let expires = pending
      .expires
      .into_iter()
      .chain(registry.windows.get(&scope).map(|window| window.expires))
      .min()
      .unwrap_or(now + WINDOW);
    let mut contexts = registry
      .windows
      .get(&scope)
      .map_or_else(HashMap::new, |window| window.contexts.clone());
    contexts.extend(
      pending
        .contexts
        .iter()
        .filter(|(name, _)| pending.used.contains(*name))
        .map(|(name, context)| (name.clone(), Arc::clone(context))),
    );
    contexts.retain(|_, context| context.is_valid());
    if contexts.is_empty() {
      return;
    }
    if !registry.windows.contains_key(&scope) && registry.windows.len() >= MAX_HOSTS {
      // Capacity pressure revokes permission; it never prolongs a window.
      if let Some(oldest) = registry
        .windows
        .iter()
        .min_by_key(|(_, window)| window.expires)
        .map(|(name, _)| name.clone())
        && let Some(window) = registry.windows.remove(&oldest)
      {
        invalidate(window.contexts.values());
        for pending in registry.pending.values_mut() {
          if pending.scope == oldest {
            pending.valid = false;
            invalidate(pending.contexts.values());
          }
        }
      }
    }
    registry.windows.insert(scope, Window { expires, contexts });
    let current = registry.pending.get_mut(&self.0.id).expect("live attempt");
    current.inherited.extend(current.used.iter().cloned());
    current.committed = true;
  }
}

impl<C: Context> Drop for Handle<C> {
  fn drop(&mut self) {
    let mut registry = self.owner.registry.lock().unwrap();
    let pending = registry.pending.remove(&self.id).expect("live attempt");
    for context in pending.contexts.values() {
      let retained = registry.windows.values().any(|window| {
        window
          .contexts
          .values()
          .any(|saved| Arc::ptr_eq(context, saved))
      });
      if !retained {
        context.invalidate();
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::cell::Cell;
  use std::sync::atomic::{AtomicBool, Ordering};

  #[test]
  fn remembered_selectors_come_only_from_committed_used_contexts() {
    let windows = Arc::new(Windows::<Fake>::default());
    let now = Instant::now();
    let attempt = windows.begin("host".into(), now);
    for (selector, present) in [("used", true), ("missing", false)] {
      attempt
        .read(
          selector,
          true,
          || Ok::<_, ()>(Fake::default()),
          || true,
          || (),
          |_, _| Ok(present.then_some(())),
        )
        .unwrap();
    }
    assert_eq!(windows.approved_selectors(), Vec::new());
    attempt.connected(now);
    let remembered = windows.approved_selectors();
    assert_eq!(
      remembered,
      vec![("host".into(), now + WINDOW, vec!["used".into()])]
    );
    windows.expire(now + WINDOW);
    assert_eq!(windows.approved_selectors(), Vec::new());
    // Keeping these selectors separately cannot extend the expired native grant.
    assert_eq!(remembered[0].2, vec!["used"]);
    assert!(
      attempt
        .read(
          "used",
          false,
          || panic!("the expired context must not be replaced"),
          || true,
          || (),
          |_, _| -> Result<Option<()>, ()> {
            panic!("the expired context must not access a credential")
          },
        )
        .is_err()
    );
    assert!(attempt.reuse_failed());
  }

  #[derive(Default)]
  struct Fake(AtomicBool);
  impl Context for Fake {
    fn invalidate(&self) {
      self.0.store(true, Ordering::SeqCst);
    }
    fn is_valid(&self) -> bool {
      !self.0.load(Ordering::SeqCst)
    }
  }
  fn read(attempt: &Attempt<Fake>, selector: &str) -> Arc<Fake> {
    let context = attempt
      .context(selector, || Ok::<_, ()>(Fake::default()))
      .unwrap()
      .unwrap();
    assert!(attempt.used(selector));
    context.context
  }

  fn decision(attempt: &Attempt<Fake>, selector: &str, interactive: bool) -> ReadDecision {
    attempt
      .read(
        selector,
        interactive,
        || Ok::<_, ()>(Fake::default()),
        || attempt.valid(),
        || (),
        |_, decision| Ok(Some(decision)),
      )
      .unwrap()
      .unwrap()
  }

  #[test]
  fn retained_host_approval_never_allows_ui_even_for_interactive_reads() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    assert_eq!(
      decision(&first, "password", true),
      ReadDecision::FreshInteractive
    );
    first.connected(start);
    let retry = store.begin("host".into(), start);
    let reused = decision(&retry, "password", true);
    assert_eq!(reused, ReadDecision::RetainedApproval);
    assert!(!reused.allow_ui());
    assert_eq!(
      decision(&first, "password", true),
      ReadDecision::RetainedApproval
    );
    assert_eq!(
      decision(&retry, "new-key", true),
      ReadDecision::FreshInteractive
    );
  }

  #[test]
  fn quiet_reads_forbid_ui_for_both_fresh_and_retained_contexts() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let fresh = decision(&first, "password", false);
    assert_eq!(fresh, ReadDecision::FreshQuiet);
    assert!(!fresh.allow_ui());
    first.connected(start);
    let retry = store.begin("host".into(), start);
    let reused = decision(&retry, "password", false);
    assert_eq!(reused, ReadDecision::RetainedApproval);
    assert!(!reused.allow_ui());
  }

  #[test]
  fn revoked_retained_attempt_requires_fresh_authorization_before_any_read() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    read(&first, "password");
    first.connected(start);
    let retry = store.begin("host".into(), start);
    assert!(store.revoke(None));
    let blocked: Result<Option<()>, _> = retry.read(
      "password",
      true,
      || panic!("a revoked attempt must not create a context"),
      || retry.valid(),
      || "unavailable",
      |_, _| panic!("a revoked attempt must not read"),
    );
    assert_eq!(blocked, Err(ReadError::unchanged("unavailable")));
    assert!(retry.reuse_failed());
  }

  #[test]
  fn failed_retained_native_read_is_not_retried_or_allowed_to_fall_back_to_ui() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let retry = store.begin("host".into(), start);
    let worker = retry.clone();
    let creations = Cell::new(0);
    let reads = Cell::new(0);
    let result: Result<Option<()>, _> = retry.read(
      "password",
      true,
      || {
        creations.set(creations.get() + 1);
        Ok(Fake::default())
      },
      || retry.valid(),
      || "unavailable",
      |_, decision| {
        reads.set(reads.get() + 1);
        assert!(!decision.allow_ui());
        Err("native denied cached authorization")
      },
    );
    assert_eq!(
      result,
      Err(ReadError {
        error: "native denied cached authorization",
        revoked: true,
      })
    );
    assert_eq!(creations.get(), 0);
    assert_eq!(reads.get(), 1);
    assert!(worker.reuse_failed());
    assert!(!context.is_valid());
    assert!(!retry.valid());
    let blocked: Result<Option<()>, _> = worker.read(
      "legacy-password",
      true,
      || {
        creations.set(creations.get() + 1);
        Ok(Fake::default())
      },
      || worker.valid(),
      || "unavailable",
      |_, _| {
        reads.set(reads.get() + 1);
        Ok(Some(()))
      },
    );
    assert_eq!(blocked, Err(ReadError::unchanged("unavailable")));
    assert_eq!(creations.get(), 0);
    assert_eq!(reads.get(), 1);
  }

  #[test]
  fn missing_retained_credential_revokes_approval_instead_of_allowing_legacy_fallback() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    decision(&first, "password", true);
    first.connected(start);
    let retry = store.begin("host".into(), start);
    let result: Result<Option<()>, _> = retry.read(
      "password",
      true,
      || panic!("a retained read must not create a context"),
      || retry.valid(),
      || "unavailable",
      |_, decision| {
        assert_eq!(decision, ReadDecision::RetainedApproval);
        Ok(None)
      },
    );
    assert_eq!(
      result,
      Err(ReadError {
        error: "unavailable",
        revoked: true,
      })
    );
    assert!(retry.reuse_failed());
    assert!(!retry.valid());
    let fresh = store.begin("host".into(), start);
    let missing: Result<Option<()>, _> = fresh.read(
      "password",
      true,
      || Ok(Fake::default()),
      || fresh.valid(),
      || "unavailable",
      |_, _| Ok(None),
    );
    assert_eq!(missing, Ok(None));
    assert_eq!(
      decision(&fresh, "legacy-password", true),
      ReadDecision::FreshInteractive
    );
  }

  #[test]
  fn revocation_and_expiry_only_report_changes_once() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    assert!(!store.revoke(None));
    let first = store.begin("host".into(), start);
    read(&first, "password");
    first.connected(start);
    assert!(store.revoke(None));
    assert!(!store.revoke(None));
    assert!(!first.cancel());
    let next = store.begin("host".into(), start);
    read(&next, "password");
    next.connected(start);
    assert!(store.expire(start + WINDOW));
    assert!(!store.expire(start + WINDOW));
    assert!(!next.revoke());
  }

  #[test]
  fn revocation_during_a_native_read_rejects_late_success() {
    let store = Arc::new(Windows::<Fake>::default());
    let attempt = store.begin("host".into(), Instant::now());
    let result = attempt.read(
      "password",
      true,
      || Ok(Fake::default()),
      || attempt.valid(),
      || "unavailable",
      |_, decision| {
        assert!(decision.allow_ui());
        store.revoke(None);
        Ok(Some("must not escape"))
      },
    );
    assert_eq!(result, Err(ReadError::unchanged("unavailable")));
    assert!(!attempt.reuse_failed());
    attempt.connected(Instant::now());
    assert!(store.registry.lock().unwrap().windows.is_empty());
  }

  #[test]
  fn a_successful_connection_reuses_authorization_without_extending_24_hours() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    drop(first);
    let second = store.begin("host".into(), start + Duration::from_hours(23));
    assert!(Arc::ptr_eq(&context, &read(&second, "password")));
    second.connected(start + Duration::from_hours(23));
    drop(second);
    let third = store.begin("host".into(), start + WINDOW);
    assert!(context.0.load(Ordering::SeqCst));
    assert!(!Arc::ptr_eq(&context, &read(&third, "password")));
  }

  #[test]
  fn failed_connections_never_authorize_a_later_attempt() {
    let store = Arc::new(Windows::<Fake>::default());
    let first = store.begin("host".into(), Instant::now());
    let context = read(&first, "password");
    drop(first);
    assert!(context.0.load(Ordering::SeqCst));
    let second = store.begin("host".into(), Instant::now());
    assert!(!Arc::ptr_eq(&context, &read(&second, "password")));
  }

  #[test]
  fn revoke_cancels_inflight_reads_and_prevents_late_success_regrant() {
    let store = Arc::new(Windows::<Fake>::default());
    let attempt = store.begin("host".into(), Instant::now());
    let context = read(&attempt, "key:version-one");
    store.revoke(None);
    assert!(context.0.load(Ordering::SeqCst));
    assert!(!attempt.used("key:version-one"));
    attempt.connected(Instant::now());
    assert!(store.registry.lock().unwrap().windows.is_empty());
  }

  #[test]
  fn authorization_is_scoped_to_host_and_exact_credential_binding() {
    let store = Arc::new(Windows::<Fake>::default());
    let first = store.begin("alice@host:route-one".into(), Instant::now());
    let context = read(&first, "key:version-one");
    first.connected(Instant::now());
    let changed = store.begin("alice@host:route-one".into(), Instant::now());
    assert!(!Arc::ptr_eq(&context, &read(&changed, "key:version-two")));
    for host in ["bob@host:route-one", "alice@host:route-two", "alice@other"] {
      let attempt = store.begin(host.into(), Instant::now());
      assert!(!Arc::ptr_eq(&context, &read(&attempt, "key:version-one")));
    }
    first.revoke();
    assert!(!first.valid());
    assert!(!changed.valid());
  }

  #[test]
  fn overlapping_first_attempts_cannot_extend_an_established_deadline() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let second = store.begin("host".into(), start);
    read(&first, "password");
    first.connected(start);
    read(&second, "password");
    second.connected(start + Duration::from_mins(1));
    assert_eq!(
      store.registry.lock().unwrap().windows["host"].expires,
      start + WINDOW
    );
  }

  #[test]
  fn disconnect_revokes_only_that_hosts_current_and_previous_settings() {
    let store = Arc::new(Windows::<Fake>::default());
    let one = store.begin("one:settings-a".into(), Instant::now());
    let two = store.begin("one:settings-b".into(), Instant::now());
    let other = store.begin("two:settings-a".into(), Instant::now());
    let one_context = read(&one, "password");
    let two_context = read(&two, "password");
    let other_context = read(&other, "password");
    one.connected(Instant::now());
    two.connected(Instant::now());
    other.connected(Instant::now());
    store.revoke_prefix("one:");
    assert!(!one.valid());
    assert!(!two.valid());
    assert!(other.valid());
    assert!(!one_context.is_valid());
    assert!(!two_context.is_valid());
    assert!(other_context.is_valid());
  }

  #[test]
  fn a_failed_native_context_is_removed_before_the_next_interactive_attempt() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let retry = store.begin("host".into(), start);
    retry.failed("password");
    assert!(!context.is_valid());
    let fresh = store.begin("host".into(), start);
    assert!(!Arc::ptr_eq(&context, &read(&fresh, "password")));
  }

  #[test]
  fn canceled_workers_cannot_grant_and_completed_reconnects_survive_cleanup() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    let worker = first.clone();
    first.cancel();
    assert!(!context.is_valid());
    assert!(!worker.valid());
    worker.connected(start);
    assert!(store.registry.lock().unwrap().windows.is_empty());
    let successful = store.begin("host".into(), start);
    let context = read(&successful, "password");
    successful.connected(start);
    successful.cancel();
    assert!(context.is_valid());
    let retry = store.begin("host".into(), start);
    assert!(Arc::ptr_eq(&context, &read(&retry, "password")));
  }

  #[test]
  fn network_failure_before_a_read_preserves_existing_approval() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let interrupted = store.begin("host".into(), start);
    interrupted.cancel();
    assert!(context.is_valid());
    let retry = store.begin("host".into(), start);
    assert!(Arc::ptr_eq(&context, &read(&retry, "password")));
  }

  #[test]
  fn expiry_also_cancels_an_attempt_that_started_before_the_deadline() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let retry = store.begin(
      "host".into(),
      (start + WINDOW)
        .checked_sub(Duration::from_secs(1))
        .unwrap(),
    );
    store.expire(start + WINDOW);
    assert!(!retry.valid());
    assert!(!context.is_valid());
    assert!(!retry.used("password"));
  }

  #[test]
  fn reconnect_preserves_approved_selectors_at_the_original_deadline() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let password = read(&first, "password");
    let key = read(&first, "key");
    first.connected(start);
    let retry = store.begin("host".into(), start);
    assert!(Arc::ptr_eq(&password, &read(&retry, "password")));
    retry.connected(start + Duration::from_hours(1));
    drop(retry);
    let another = store.begin("host".into(), start);
    assert!(Arc::ptr_eq(&key, &read(&another, "key")));
    assert_eq!(
      store.registry.lock().unwrap().windows["host"].expires,
      start + WINDOW
    );
  }

  #[test]
  fn capacity_eviction_also_revokes_pending_reconnects() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("oldest".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let pending = store.begin("oldest".into(), start);
    read(&pending, "password");
    for index in 1..=MAX_HOSTS {
      let next = store.begin(format!("host-{index}"), start);
      read(&next, "password");
      next.connected(start + Duration::from_secs(index as u64));
    }
    assert!(!context.is_valid());
    assert!(!pending.valid());
    pending.connected(start + Duration::from_hours(1));
    assert!(
      !store
        .registry
        .lock()
        .unwrap()
        .windows
        .contains_key("oldest")
    );
  }

  #[test]
  fn host_confirmation_discards_old_contexts_but_allows_fresh_authorization() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    let retry = store.begin("host".into(), start);
    retry.discard_cached();
    assert!(!context.is_valid());
    assert!(!first.valid());
    assert!(retry.valid());
    assert!(!Arc::ptr_eq(&context, &read(&retry, "password")));
    retry.cancel();
    retry.discard_cached();
    assert!(!retry.valid());
  }

  #[test]
  fn restoring_previous_settings_does_not_restore_revoked_approval() {
    let store = Arc::new(Windows::<Fake>::default());
    let start = Instant::now();
    let first = store.begin("host:old".into(), start);
    let context = read(&first, "password");
    first.connected(start);
    store.select_scope("host:", Some("host:new"));
    store.select_scope("host:", Some("host:old"));
    let restored = store.begin("host:old".into(), start);
    assert!(!context.is_valid());
    assert!(!Arc::ptr_eq(&context, &read(&restored, "password")));
    store.select_scope("host:", None);
    assert!(!restored.valid());
  }
}
