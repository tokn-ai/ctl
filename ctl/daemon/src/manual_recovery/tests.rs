use super::*;
use std::sync::Arc;

fn target() -> SshTarget {
  SshTarget {
    destination: "fixture-alias".into(),
    ssh_config_alias: None,
    use_ssh_config_master: Some(false),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

fn shared(private: &SshTarget) -> SshTarget {
  SshTarget {
    ssh_config_alias: Some(private.destination.clone()),
    use_ssh_config_master: Some(true),
    ..private.clone()
  }
}

#[tokio::test]
async fn exact_ready_master_notifies_all_live_same_epoch_shells_without_approval() {
  let state = Arc::new(State::default());
  let target = target();
  let (_first, first) = state.wake_recovery.register(target.clone(), 7).unwrap();
  let (_second, second) = state.wake_recovery.register(target.clone(), 7).unwrap();
  let old = state.target(&target).attempt();
  state.target(&target).require_authentication(&old);
  Capture::new(&state, &target)
    .prepare(&state)
    .await
    .publish(&state);
  assert_eq!(*first.borrow(), Some(1));
  assert_eq!(*second.borrow(), Some(1));
  state.target(&target).require_authentication(&old);
  assert!(state.target(&target).allow_background_connection().is_ok());
  assert_eq!(
    state
      .existing_endpoint(&target)
      .unwrap()
      .unwrap()
      .control_path,
    crate::control_path(&target)
  );
}

#[test]
fn equivalent_master_policy_requires_reusable_scoped_approval_and_preserves_paths() {
  for reusable in [false, true] {
    let state = State::default();
    let private = target();
    let shared = shared(&private);
    let (_private, private_updates) = state.wake_recovery.register(private.clone(), 0).unwrap();
    let (_shared, shared_updates) = state.wake_recovery.register(shared.clone(), 0).unwrap();
    let old = state.target(&private).attempt();
    state.target(&private).require_authentication(&old);
    Capture::new(&state, &shared)
      .prepared(|_| reusable)
      .publish(&state);
    assert!(shared_updates.borrow().is_some());
    assert_eq!(private_updates.borrow().is_some(), reusable);
    state.target(&private).require_authentication(&old);
    assert_eq!(
      state.target(&private).allow_background_connection().is_ok(),
      reusable
    );
    assert_ne!(crate::control_path(&private), crate::control_path(&shared));
    assert!(state.endpoint(&shared).is_none());
  }
}

#[test]
fn revoked_prepared_approval_suppresses_counterparts_but_not_the_exact_ready_master() {
  let state = State::default();
  let source = target();
  let counterpart = shared(&source);
  let (_exact, exact) = state.wake_recovery.register(source.clone(), 0).unwrap();
  let (_other, other) = state
    .wake_recovery
    .register(counterpart.clone(), 0)
    .unwrap();
  let old = state.target(&counterpart).attempt();
  state.target(&counterpart).require_authentication(&old);
  let ready = Capture::new(&state, &source).prepared(|_| true);
  // A committed scope observed during prepare may expire or be revoked before
  // ownership commits. Its quiet-retry notification must then be suppressed.
  ready.publish_using(&state, || false);
  assert_eq!(*exact.borrow(), Some(1));
  assert_eq!(*other.borrow(), None);
  assert!(
    state
      .target(&counterpart)
      .allow_background_connection()
      .is_err()
  );
}

#[test]
fn endpoint_account_port_identity_and_route_distinctions_never_share_events() {
  let state = State::default();
  let source = target();
  let variants = [
    SshTarget {
      user: Some("other".into()),
      ..source.clone()
    },
    SshTarget {
      port: Some(2222),
      ..source.clone()
    },
    SshTarget {
      hostname: Some("other.invalid".into()),
      ..source.clone()
    },
    SshTarget {
      identity_file: Some("/fixture/other-key".into()),
      ..source.clone()
    },
    SshTarget {
      gateways: vec![ssh_gateway("other")],
      ..source.clone()
    },
  ];
  let watches: Vec<_> = variants
    .iter()
    .map(|target| state.wake_recovery.register(target.clone(), 0).unwrap())
    .collect();
  Capture::new(&state, &source)
    .prepared(|_| true)
    .publish(&state);
  assert!(
    watches
      .iter()
      .all(|(_, updates)| updates.borrow().is_none())
  );
}

#[test]
fn source_supersession_closed_shells_and_counterpart_pauses_suppress_publication() {
  for source_changed in [false, true] {
    let state = State::default();
    let source = target();
    let counterpart = shared(&source);
    let (source_lease, source_updates) = state.wake_recovery.register(source.clone(), 0).unwrap();
    let (_counterpart, counterpart_updates) = state
      .wake_recovery
      .register(counterpart.clone(), 0)
      .unwrap();
    let recovery = Capture::new(&state, &source).prepared(|_| true);
    if source_changed {
      let source = state.target(&source);
      source.pause();
      source.resume(&mut source.attempt()).unwrap();
    } else {
      drop(source_lease);
      state.target(&counterpart).pause();
    }
    recovery.publish(&state);
    assert_eq!(*source_updates.borrow(), None);
    assert_eq!(*counterpart_updates.borrow(), None);
  }
}

#[test]
fn a_counterpart_paused_before_capture_is_not_resumed_by_another_hosts_connect() {
  let state = State::default();
  let source = target();
  let counterpart = shared(&source);
  let (_lease, updates) = state
    .wake_recovery
    .register(counterpart.clone(), 0)
    .unwrap();
  state.target(&counterpart).pause();
  let recovery = Capture::new(&state, &source).prepared(|_| true);
  // Even a separate explicit resume cannot make this earlier capture select it.
  let lifecycle = state.target(&counterpart);
  lifecycle.resume(&mut lifecycle.attempt()).unwrap();
  lifecycle.require_authentication(&lifecycle.attempt());
  recovery.publish(&state);
  assert_eq!(*updates.borrow(), None);
  assert!(lifecycle.allow_background_connection().is_err());
}

fn ssh_gateway(destination: &str) -> ctl_ipc::SshGateway {
  ctl_ipc::SshGateway {
    kind: ctl_ipc::GatewayKind::Ssh,
    vpn: None,
    destination: destination.into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctl_ipc::SshGatewayMode::Automatic,
  }
}

fn dependent(destination: &str) -> SshTarget {
  SshTarget {
    destination: destination.into(),
    gateways: vec![
      ssh_gateway("vpn-owner"),
      ctl_ipc::SshGateway {
        kind: ctl_ipc::GatewayKind::Vpn,
        vpn: Some(ctl_ipc::VpnGateway {
          connection_id: "fixture-profile".into(),
          socket_path: "/fixture/vpn.sock".into(),
          expected_remote_id: None,
        }),
        ..ssh_gateway("fixture-profile")
      },
    ],
    ..target()
  }
}

#[test]
fn repeated_dependents_snapshot_their_shared_owner_only_once_even_when_unusable() {
  let state = State::default();
  let first = dependent("dependent-0");
  let owner = ctl_ipc::vpn_owner_target(&first.gateways, 1).unwrap();
  let watches: Vec<_> = (0..32)
    .map(|index| {
      state
        .wake_recovery
        .register(dependent(&format!("dependent-{index}")), 0)
        .unwrap()
    })
    .collect();
  let capture = Capture::new(&state, &owner);
  // Deduplication precedes snapshots, so failed/mismatching observations cannot
  // repeat ssh -G or trust reads for each dependent shell.
  assert_eq!(capture.approval_routes(), vec![owner]);
  capture.prepared(|_| false).publish(&state);
  assert!(
    watches
      .iter()
      .all(|(_, updates)| updates.borrow().is_none())
  );
}

#[test]
fn remote_vpn_owner_can_resume_its_live_dependents_with_reusable_approval() {
  for reusable in [false, true] {
    let state = State::default();
    let host = dependent("dependent");
    let owner = ctl_ipc::vpn_owner_target(&host.gateways, 1).unwrap();
    let (_host, updates) = state.wake_recovery.register(host.clone(), 9).unwrap();
    let old_host = state.target(&host).attempt();
    let old_owner = state.target(&owner).attempt();
    state.target(&host).require_authentication(&old_host);
    state.target(&owner).require_authentication(&old_owner);
    Capture::new(&state, &owner)
      .prepared(|_| reusable)
      .publish(&state);
    assert_eq!(updates.borrow().is_some(), reusable);
    state.target(&host).require_authentication(&old_host);
    state.target(&owner).require_authentication(&old_owner);
    assert_eq!(
      state.target(&host).allow_background_connection().is_ok(),
      reusable
    );
    assert_eq!(
      state.target(&owner).allow_background_connection().is_ok(),
      reusable
    );
  }
}

#[cfg(unix)]
#[tokio::test]
async fn reused_interactive_success_notifies_before_reply_but_quiet_reuse_does_not() {
  for interactive in [false, true] {
    let state = Arc::new(State::default());
    let target = target();
    let (_lease, updates) = state.wake_recovery.register(target.clone(), 0).unwrap();
    let recovery = if interactive {
      Some(Capture::new(&state, &target).prepare(&state).await)
    } else {
      None
    };
    let path = crate::control_path(&target);
    let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
    crate::publish_reused_master_ready(&mut server, &state, path.clone(), recovery)
      .await
      .unwrap();
    assert_eq!(updates.borrow().is_some(), interactive);
    assert!(matches!(
      ctl_ipc::read_frame::<_, ctl_ipc::ServerMessage>(&mut client).await.unwrap(),
      Some(ctl_ipc::ServerMessage::MasterReady { control_path }) if control_path == path
    ));
    drop(client);
    assert_eq!(updates.borrow().is_some(), interactive);
  }
}
