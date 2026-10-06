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

struct Window<C> {
  expires: Instant,
  contexts: HashMap<String, Arc<C>>,
}

struct Pending<C> {
  scope: String,
  valid: bool,
  expires: Option<Instant>,
  contexts: HashMap<String, Arc<C>>,
  used: HashSet<String>,
  reading: HashSet<String>,
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
  pub(crate) fn begin(self: &Arc<Self>, scope: String, now: Instant) -> Attempt<C> {
    self.expire(now);
    let mut registry = self.registry.lock().unwrap();
    let (expires, contexts) = registry.windows.get(&scope).map_or_else(
      || (None, HashMap::new()),
      |window| (Some(window.expires), window.contexts.clone()),
    );
    let id = registry.next;
    registry.next = registry.next.checked_add(1).expect("bounded attempt IDs");
    registry.pending.insert(
      id,
      Pending {
        scope,
        valid: true,
        expires,
        contexts,
        used: HashSet::new(),
        reading: HashSet::new(),
        committed: false,
      },
    );
    Attempt(Arc::new(Handle {
      owner: Arc::clone(self),
      id,
    }))
  }

  pub(crate) fn revoke(&self, scope: Option<&str>) {
    self.revoke_matching(|name| scope.is_none_or(|scope| scope == name));
  }

  pub(crate) fn revoke_prefix(&self, prefix: &str) {
    self.revoke_matching(|name| name.starts_with(prefix));
  }

  pub(crate) fn select_scope(&self, prefix: &str, current: Option<&str>) {
    self.revoke_matching(|name| name.starts_with(prefix) && Some(name) != current);
  }

  fn revoke_matching(&self, matches: impl Fn(&str) -> bool) {
    let mut registry = self.registry.lock().unwrap();
    registry.windows.retain(|name, window| {
      if matches(name) {
        invalidate(window.contexts.values());
        false
      } else {
        true
      }
    });
    for pending in registry.pending.values_mut() {
      if matches(&pending.scope) {
        pending.valid = false;
        invalidate(pending.contexts.values());
      }
    }
  }

  pub(crate) fn expire(&self, now: Instant) {
    let mut registry = self.registry.lock().unwrap();
    registry.windows.retain(|_, window| {
      window.contexts.retain(|_, context| context.is_valid());
      if window.contexts.is_empty() {
        return false;
      }
      if now >= window.expires {
        invalidate(window.contexts.values());
        false
      } else {
        true
      }
    });
    for pending in registry.pending.values_mut() {
      if pending.expires.is_some_and(|expires| now >= expires) {
        pending.valid = false;
        invalidate(pending.contexts.values());
      }
    }
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
  pub(crate) fn context<E>(
    &self,
    selector: &str,
    create: impl FnOnce() -> Result<C, E>,
  ) -> Result<Option<Arc<C>>, E> {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = registry.pending.get_mut(&self.0.id).expect("live attempt");
    if !pending.valid {
      return Ok(None);
    }
    if let Some(context) = pending.contexts.get(selector) {
      pending.reading.insert(selector.into());
      return Ok(Some(Arc::clone(context)));
    }
    if pending.contexts.len() >= MAX_CONTEXTS {
      return Ok(None);
    }
    let context = Arc::new(create()?);
    pending
      .contexts
      .insert(selector.into(), Arc::clone(&context));
    pending.reading.insert(selector.into());
    Ok(Some(context))
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

  pub(crate) fn failed(&self, selector: &str) {
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
    self.revoke();
  }

  /// A host confirmation requires fresh native authorization. Clear reusable
  /// contexts atomically, without reviving a canceled connection attempt.
  pub(crate) fn discard_cached(&self) {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = &registry.pending[&self.0.id];
    if !pending.valid {
      return;
    }
    let scope = pending.scope.clone();
    if let Some(window) = registry.windows.remove(&scope) {
      invalidate(window.contexts.values());
    }
    for pending in registry.pending.values_mut() {
      if pending.scope == scope {
        invalidate(pending.contexts.values());
        pending.valid = false;
      }
    }
    let current = registry.pending.get_mut(&self.0.id).expect("live attempt");
    current.contexts.clear();
    current.used.clear();
    current.reading.clear();
    current.expires = None;
    current.committed = false;
    current.valid = true;
  }

  pub(crate) fn revoke(&self) {
    let scope = self.0.owner.registry.lock().unwrap().pending[&self.0.id]
      .scope
      .clone();
    self.0.owner.revoke(Some(&scope));
  }

  pub(crate) fn cancel(&self) {
    let mut registry = self.0.owner.registry.lock().unwrap();
    let pending = &registry.pending[&self.0.id];
    if pending.committed {
      return;
    }
    for (name, context) in &pending.contexts {
      let retained = registry.windows.get(&pending.scope).is_some_and(|window| {
        window
          .contexts
          .get(name)
          .is_some_and(|saved| Arc::ptr_eq(saved, context))
      });
      if pending.reading.contains(name) || !retained {
        context.invalidate();
      }
    }
    registry
      .pending
      .get_mut(&self.0.id)
      .expect("live attempt")
      .valid = false;
  }

  /// Only SSH success commits permission. Previously approved windows retain
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
    registry
      .pending
      .get_mut(&self.0.id)
      .expect("live attempt")
      .committed = true;
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
  use std::sync::atomic::{AtomicBool, Ordering};

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
    context
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
