use super::*;

fn targets() -> (SshTarget, SshTarget) {
  let private = SshTarget {
    destination: "fixture-alias".into(),
    ssh_config_alias: None,
    use_ssh_config_master: Some(false),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  };
  let mut shared = private.clone();
  shared.ssh_config_alias = Some(shared.destination.clone());
  shared.use_ssh_config_master = Some(true);
  (private, shared)
}

fn scope(state: &State, members: &[SshTarget]) -> Scope {
  Scope {
    target: members[0].clone(),
    name: "fixture-approval".into(),
    prefix: approval_scope::prefix(&members[0]),
    selectors: vec!["fixture-key".into()],
    members: members
      .iter()
      .map(|target| (target.clone(), state.target(target).attempt()))
      .collect(),
  }
}

#[test]
fn one_approval_scope_publishes_to_each_exact_shared_and_private_lifecycle() {
  let state = State::default();
  let (private, shared) = targets();
  assert_eq!(
    approval_scope::prefix(&private),
    approval_scope::prefix(&shared)
  );
  assert_ne!(crate::target_key(&private), crate::target_key(&shared));
  let hosts = [private.clone(), shared.clone()];
  let (_private_lease, private_updates) = state.wake_recovery.register(private.clone(), 0).unwrap();
  let (_shared_lease, shared_updates) = state.wake_recovery.register(shared.clone(), 0).unwrap();
  for host in &hosts {
    state
      .target(host)
      .require_authentication(&state.target(host).attempt());
    assert!(state.target(host).allow_background_connection().is_err());
  }
  publish(&state, &hosts, &[scope(&state, &hosts)], 1);
  assert!(state.target(&private).allow_background_connection().is_ok());
  assert!(state.target(&shared).allow_background_connection().is_ok());
  assert_eq!(*private_updates.borrow(), Some(1));
  assert_eq!(*shared_updates.borrow(), Some(1));
}

#[test]
fn publication_never_undoes_a_manual_pause_or_a_newer_explicit_connection() {
  for newer_connect in [false, true] {
    let state = State::default();
    let (private, shared) = targets();
    let hosts = [private.clone(), shared.clone()];
    let (_private_lease, private_updates) =
      state.wake_recovery.register(private.clone(), 0).unwrap();
    let (_shared_lease, shared_updates) = state.wake_recovery.register(shared.clone(), 0).unwrap();
    let scopes = [scope(&state, &hosts)];
    for host in &hosts {
      state
        .target(host)
        .require_authentication(&state.target(host).attempt());
    }
    let lifecycle = state.target(&private);
    lifecycle.pause();
    if newer_connect {
      lifecycle.resume(&mut lifecycle.attempt()).unwrap();
      lifecycle.require_authentication(&lifecycle.attempt());
    }
    publish(&state, &hosts, &scopes, 1);
    assert!(lifecycle.allow_background_connection().is_err());
    assert_eq!(lifecycle.is_paused(), !newer_connect);
    assert_eq!(*private_updates.borrow(), None);
    assert!(state.target(&shared).allow_background_connection().is_ok());
    assert_eq!(*shared_updates.borrow(), Some(1));
  }
}

#[test]
fn closed_or_post_sleep_registrations_cannot_receive_an_earlier_batch() {
  let state = State::default();
  let (private, _) = targets();
  let hosts = [private.clone()];
  let (old, _) = state.wake_recovery.register(private.clone(), 0).unwrap();
  let scopes = [scope(&state, &hosts)];
  state
    .target(&private)
    .require_authentication(&state.target(&private).attempt());
  drop(old);
  let (_new, updates) = state.wake_recovery.register(private.clone(), 1).unwrap();
  publish(&state, &hosts, &scopes, 1);
  assert!(
    state
      .target(&private)
      .allow_background_connection()
      .is_err()
  );
  assert_eq!(*updates.borrow(), None);
}
