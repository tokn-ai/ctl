use super::*;

#[test]
fn unmonitored_state_fails_closed() {
  let state = SessionState::new();
  assert!(!state.is_unlocked());
}

#[test]
fn revocation_updates_generation_and_removes_subscriptions() {
  let state = Arc::new(SessionState::new());
  let revoked = Arc::new(AtomicU64::new(0));
  let count = Arc::clone(&revoked);
  let guard = state.subscribe(Arc::new(move || {
    count.fetch_add(1, Ordering::Relaxed);
  }));
  state.revoke();
  assert_eq!(state.generation(), 1);
  assert_eq!(revoked.load(Ordering::Relaxed), 1);
  drop(guard);
  state.revoke();
  assert_eq!(revoked.load(Ordering::Relaxed), 1);
}

#[test]
fn panicking_subscription_disables_monitor_and_preserves_other_revocations() {
  let state = Arc::new(SessionState::new());
  state.available.store(true, Ordering::Release);
  let revoked = Arc::new(AtomicBool::new(false));
  let count = Arc::clone(&revoked);
  let _first = state.subscribe(Arc::new(|| panic!("subscriber failed")));
  let _second = state.subscribe(Arc::new(move || count.store(true, Ordering::Relaxed)));
  state.revoke();
  assert!(!state.available.load(Ordering::Acquire));
  assert!(revoked.load(Ordering::Relaxed));
}

#[test]
fn secure_input_metadata_changes_do_not_revoke_approval() {
  let state = Arc::new(SessionState::new());
  state.unlocked.store(true, Ordering::Release);
  let revoked = Arc::new(AtomicU64::new(0));
  let count = Arc::clone(&revoked);
  let _guard = state.subscribe(Arc::new(move || {
    count.fetch_add(1, Ordering::Relaxed);
  }));
  state.console_changed(true);
  assert_eq!(state.generation(), 0);
  assert_eq!(revoked.load(Ordering::Relaxed), 0);
  state.console_changed(false);
  assert_eq!(state.generation(), 1);
  assert_eq!(revoked.load(Ordering::Relaxed), 1);
}

#[test]
fn delayed_lock_event_revokes_after_console_has_unlocked() {
  let state = Arc::new(SessionState::new());
  state.unlocked.store(true, Ordering::Release);
  let revoked = Arc::new(AtomicU64::new(0));
  let count = Arc::clone(&revoked);
  let _guard = state.subscribe(Arc::new(move || {
    count.fetch_add(1, Ordering::Relaxed);
  }));
  let mut callback = CallbackState {
    state: Arc::clone(&state),
    connection: 0,
    probe: String::new(),
  };
  let name = CFString::new("com.apple.screenIsLocked");
  distributed_event(
    ptr::null_mut(),
    (&raw mut callback).cast::<c_void>(),
    name.as_concrete_TypeRef(),
    ptr::null(),
    ptr::null(),
  );
  assert_eq!(state.generation(), 1);
  assert_eq!(revoked.load(Ordering::Relaxed), 1);
  assert!(!state.unlocked.load(Ordering::Acquire));
}

fn resume_fixture() -> (
  Arc<SessionState>,
  Arc<Mutex<Vec<SessionResume>>>,
  SessionSubscription,
) {
  let state = Arc::new(SessionState::new());
  state.available.store(true, Ordering::Release);
  let events = Arc::new(Mutex::new(Vec::new()));
  let received = Arc::clone(&events);
  let subscription = state.subscribe_resume(Arc::new(move |event| {
    received.lock().unwrap().push(event);
  }));
  (state, events, subscription)
}

#[test]
fn dark_wakes_and_unlocks_without_a_display_wake_do_not_resume() {
  let (state, events, _subscription) = resume_fixture();
  state.will_sleep();
  state.powered_on(true);
  state.console_available(true);
  state.console_changed(true);
  assert!(events.lock().unwrap().is_empty());
  state.screens_changed(true, true);
  assert_eq!(
    *events.lock().unwrap(),
    vec![SessionResume { generation: 1 }]
  );
}

#[test]
fn screens_waking_while_locked_wait_for_the_subsequent_unlock() {
  let (state, events, _subscription) = resume_fixture();
  state.will_sleep();
  state.screens_changed(true, false);
  assert!(events.lock().unwrap().is_empty());
  state.revoke();
  state.console_available(true);
  assert_eq!(
    *events.lock().unwrap(),
    vec![SessionResume { generation: 2 }]
  );
  assert_eq!(state.generation(), 2);
}

#[test]
fn resume_is_once_per_sleep_epoch_and_ignores_ordinary_unlocks() {
  let (state, events, _subscription) = resume_fixture();
  state.screens_changed(true, true);
  state.console_available(true);
  assert!(events.lock().unwrap().is_empty());
  state.will_sleep();
  state.screens_changed(true, true);
  state.console_available(true);
  state.screens_changed(true, true);
  state.revoke();
  state.console_available(true);
  assert_eq!(events.lock().unwrap().len(), 1);
  state.will_sleep();
  state.screens_changed(true, true);
  assert_eq!(
    *events.lock().unwrap(),
    vec![
      SessionResume { generation: 1 },
      SessionResume { generation: 3 }
    ]
  );
}

#[test]
fn a_second_sleep_and_screen_sleep_require_another_display_wake() {
  let (state, events, _subscription) = resume_fixture();
  state.will_sleep();
  state.screens_changed(true, false);
  state.screens_changed(false, false);
  state.console_available(true);
  assert!(events.lock().unwrap().is_empty());
  state.screens_changed(true, false);
  state.will_sleep();
  state.console_available(true);
  assert!(events.lock().unwrap().is_empty());
  state.screens_changed(true, true);
  assert_eq!(
    *events.lock().unwrap(),
    vec![SessionResume { generation: 2 }]
  );
}

#[test]
fn unknown_monitor_state_never_emits_resume_and_dropping_removes_the_callback() {
  let (state, events, subscription) = resume_fixture();
  state.available.store(false, Ordering::Release);
  state.will_sleep();
  state.screens_changed(true, true);
  assert!(events.lock().unwrap().is_empty());
  drop(subscription);
  state.available.store(true, Ordering::Release);
  state.console_available(true);
  assert!(events.lock().unwrap().is_empty());
  assert!(state.resume_subscribers.lock().unwrap().is_empty());
}

#[test]
fn queued_resume_events_reject_a_later_lock_and_display_sleep() {
  let (state, events, _subscription) = resume_fixture();
  state.will_sleep();
  state.screens_changed(true, true);
  let event = events.lock().unwrap()[0];
  state.screens_changed(false, true);
  assert!(!state.resume_is_current(event));
  state.revoke();
  state.screens_changed(true, true);
  assert!(!state.resume_is_current(event));
  assert_eq!(events.lock().unwrap().len(), 1);
}

#[test]
fn resume_callbacks_can_manage_subscriptions_without_holding_the_registry_lock() {
  let (state, events, subscription) = resume_fixture();
  let subscription = Arc::new(Mutex::new(Some(subscription)));
  let removed = Arc::clone(&subscription);
  let _callback = state.subscribe_resume(Arc::new(move |_| {
    removed.lock().unwrap().take();
  }));
  state.will_sleep();
  state.screens_changed(true, true);
  assert_eq!(events.lock().unwrap().len(), 1);
  state.will_sleep();
  state.screens_changed(true, true);
  assert_eq!(events.lock().unwrap().len(), 1);
}

#[test]
fn panicking_resume_callback_revokes_permission_and_invalidates_queued_events() {
  let (state, events, _subscription) = resume_fixture();
  let _callback = state.subscribe_resume(Arc::new(|_| panic!("resume callback failed")));
  let revoked = Arc::new(AtomicU64::new(0));
  let received = Arc::clone(&revoked);
  let _revocation = state.subscribe(Arc::new(move || {
    received.fetch_add(1, Ordering::Relaxed);
  }));
  state.will_sleep();
  state.screens_changed(true, true);
  assert!(!state.available.load(Ordering::Acquire));
  assert_eq!(revoked.load(Ordering::Relaxed), 2);
  assert_eq!(events.lock().unwrap()[0].generation, 1);
  assert_eq!(state.generation(), 2);
}
