use std::collections::HashSet;
use std::future::ready;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct Change {
  target: SshTarget,
  forward: LocalPortForward,
  cancel: bool,
}

#[derive(Default)]
struct Control {
  ready: Mutex<HashSet<SshTarget>>,
  changes: Mutex<Vec<Change>>,
  fail_cancel: AtomicBool,
  fail_check: AtomicBool,
}

impl Control {
  fn ready(targets: &[SshTarget]) -> Self {
    Self {
      ready: Mutex::new(targets.iter().cloned().collect()),
      ..Self::default()
    }
  }
}

impl ForwardControl for Control {
  fn is_ready(&self, target: &SshTarget) -> impl Future<Output = bool> + Send {
    ready(self.ready.lock().unwrap().contains(target))
  }

  fn check(
    &self,
    _target: &SshTarget,
    _forward: &LocalPortForward,
  ) -> impl Future<Output = Result<(), RequestError>> + Send {
    ready(if self.fail_check.load(Ordering::SeqCst) {
      Err(RequestError::PortForwardFailed(
        "SSH transport exited".into(),
      ))
    } else {
      Ok(())
    })
  }

  fn change(
    &self,
    target: &SshTarget,
    forward: &LocalPortForward,
    cancel: bool,
  ) -> impl Future<Output = Result<(), RequestError>> + Send {
    self.changes.lock().unwrap().push(Change {
      target: target.clone(),
      forward: forward.clone(),
      cancel,
    });
    if !cancel {
      self.fail_check.store(false, Ordering::SeqCst);
    }
    ready(if cancel && self.fail_cancel.load(Ordering::SeqCst) {
      Err(RequestError::PortForwardFailed("cancel failed".into()))
    } else {
      Ok(())
    })
  }
}

#[tokio::test]
async fn passive_health_failure_reports_error_and_explicit_configure_replaces_the_dead_owner() {
  let target = target("native-host");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  control.fail_check.store(true, Ordering::SeqCst);
  let statuses = registry.list(&control, &target).await;
  assert_eq!(statuses[0].state, PortForwardState::Error);
  assert!(
    statuses[0]
      .message
      .as_ref()
      .unwrap()
      .contains("SSH transport exited")
  );
  assert_eq!(control.changes.lock().unwrap().len(), 1);
  let status = registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  assert_eq!(status.state, PortForwardState::Active);
  let changes = control.changes.lock().unwrap();
  assert_eq!(changes.len(), 3);
  assert!(changes[1].cancel);
  assert!(!changes[2].cancel);
}

fn target(destination: &str) -> SshTarget {
  SshTarget {
    destination: destination.into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: Some("developer".into()),
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

fn forward() -> LocalPortForward {
  LocalPortForward {
    forward_id: "workspace-forward-id".into(),
    bind_address: "127.0.0.1".into(),
    local_port: 15432,
    remote_host: "127.0.0.1".into(),
    remote_port: 5432,
  }
}

#[tokio::test]
async fn failed_mux_add_identifies_an_occupied_port_without_touching_its_listener() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = LocalPortForward {
    local_port: listener.local_addr().unwrap().port(),
    ..forward()
  };
  let original = "mux_client_forward: forwarding request failed: Port forwarding failed";
  let error = forward_command_error(&forward, original.into(), false).await;
  assert_eq!(error.code(), "ssh_port_forward_failed");
  let message = error.to_string();
  assert!(message.contains(&format!(
    "Local address 127.0.0.1:{} is already in use",
    forward.local_port
  )));
  assert!(message.contains("owning ctld/application"));
  assert!(message.contains("choose another local port"));
  assert!(message.contains(original));

  let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
    .await
    .unwrap();
  let (accepted, _) = tokio::time::timeout(std::time::Duration::from_secs(1), listener.accept())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(client.local_addr().unwrap(), accepted.peer_addr().unwrap());
}

#[tokio::test]
async fn failed_mux_cancel_preserves_its_error_even_when_the_port_is_occupied() {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = LocalPortForward {
    local_port: listener.local_addr().unwrap().port(),
    ..forward()
  };
  let original = "mux_client_forward: cancellation request failed";
  let error = forward_command_error(&forward, original.into(), true).await;
  let RequestError::PortForwardFailed(message) = error else {
    panic!("expected original forwarding error");
  };
  assert_eq!(message, original);
}

#[tokio::test]
async fn other_mux_failures_are_not_misclassified_as_a_port_conflict() {
  let forward = LocalPortForward {
    // A bind failure other than AddrInUse must preserve the mux failure.
    bind_address: "192.0.2.1".into(),
    ..forward()
  };
  let original = "Control socket connect: Connection refused";
  let RequestError::PortForwardFailed(message) =
    forward_command_error(&forward, original.into(), false).await
  else {
    panic!("expected original forwarding error");
  };
  assert_eq!(message, original);
}

#[test]
fn bind_failures_include_the_exact_endpoint_and_only_report_real_conflicts() {
  let forward = LocalPortForward {
    bind_address: "::1".into(),
    ..forward()
  };
  let conflict = local_bind_error(&forward, &io::Error::from(io::ErrorKind::AddrInUse));
  assert!(
    conflict
      .to_string()
      .contains("Local address [::1]:15432 is already in use")
  );
  let denied = local_bind_error(&forward, &io::Error::from(io::ErrorKind::PermissionDenied));
  assert!(denied.to_string().contains("Cannot listen on [::1]:15432"));
  assert!(!denied.to_string().contains("already in use"));
}

#[tokio::test]
async fn shutdown_cancels_only_owned_listeners_and_reports_unreleased_records() {
  let target = target("test-shutdown");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  control.changes.lock().unwrap().clear();
  control.fail_cancel.store(true, Ordering::SeqCst);
  assert!(!registry.shutdown(&control).await);
  assert!(registry.records[&forward().forward_id].listener_present);
  control.fail_cancel.store(false, Ordering::SeqCst);
  assert!(registry.shutdown(&control).await);
  assert!(!registry.records[&forward().forward_id].listener_present);
  assert_eq!(control.changes.lock().unwrap().len(), 2);
  assert!(
    control
      .changes
      .lock()
      .unwrap()
      .iter()
      .all(|change| change.cancel && change.target == target && change.forward == forward())
  );
  assert!(registry.shutdown(&control).await);
  assert_eq!(control.changes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn moving_a_forward_cancels_its_exact_previous_route_before_starting_the_new_one() {
  let old = target("office-network");
  let mut next = target("preferred-route");
  next.gateways.push(ctl_ipc::SshGateway {
    kind: ctl_ipc::GatewayKind::Ssh,
    vpn: None,
    destination: "bastion".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctl_ipc::SshGatewayMode::Automatic,
  });
  let control = Control::ready(&[old.clone(), next.clone()]);
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, old.clone(), forward(), true)
    .await
    .unwrap();
  let status = registry
    .configure(&control, next.clone(), forward(), true)
    .await
    .unwrap();
  assert_eq!(status.state, PortForwardState::Active);
  assert_eq!(
    control.changes.lock().unwrap().as_slice(),
    &[
      Change {
        target: old.clone(),
        forward: forward(),
        cancel: false
      },
      Change {
        target: old.clone(),
        forward: forward(),
        cancel: true
      },
      Change {
        target: next.clone(),
        forward: forward(),
        cancel: false
      },
    ]
  );
  assert_eq!(
    registry.list(&control, &old).await,
    Vec::<PortForwardStatus>::new()
  );
  assert_eq!(registry.list(&control, &next).await, vec![status.clone()]);
  // Replaying startup restoration is idempotent, and a later authentication
  // through the obsolete route cannot recreate its removed forward.
  registry
    .configure(&control, next, forward(), true)
    .await
    .unwrap();
  registry.activate(&control, &old).await;
  assert_eq!(control.changes.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn disable_uses_stored_owner_and_definition_even_after_the_method_was_deleted() {
  let old = target("removed-route");
  let current = target("current-route");
  let control = Control::ready(std::slice::from_ref(&old));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, old.clone(), forward(), true)
    .await
    .unwrap();
  let changed = LocalPortForward {
    local_port: 25432,
    ..forward()
  };
  registry
    .configure(&control, current, changed, false)
    .await
    .unwrap();
  assert_eq!(
    control.changes.lock().unwrap()[1],
    Change {
      target: old.clone(),
      forward: forward(),
      cancel: true,
    }
  );
  registry.activate(&control, &old).await;
  assert_eq!(
    registry.records.keys().collect::<Vec<_>>(),
    Vec::<&String>::new()
  );
  assert_eq!(control.changes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn failed_cancellation_keeps_the_previous_owner_for_both_move_and_disable() {
  for enabled in [true, false] {
    let old = target("old");
    let next = target("new");
    let control = Control::ready(&[old.clone(), next.clone()]);
    let mut registry = ForwardRegistry::default();
    let original = registry
      .configure(&control, old.clone(), forward(), true)
      .await
      .unwrap();
    control.fail_cancel.store(true, Ordering::SeqCst);
    assert!(
      registry
        .configure(&control, next.clone(), forward(), enabled)
        .await
        .is_err()
    );
    assert_eq!(registry.list(&control, &old).await, vec![original]);
    assert_eq!(
      registry.list(&control, &next).await,
      Vec::<PortForwardStatus>::new()
    );
    assert_eq!(control.changes.lock().unwrap().len(), 2);
    control.fail_cancel.store(false, Ordering::SeqCst);
    registry
      .configure(&control, next.clone(), forward(), enabled)
      .await
      .unwrap();
    assert_eq!(
      registry.list(&control, &old).await,
      Vec::<PortForwardStatus>::new()
    );
    assert_eq!(
      registry.list(&control, &next).await.len(),
      usize::from(enabled)
    );
  }
}

#[tokio::test]
async fn moving_a_waiting_forward_prevents_the_old_master_from_activating_it_later() {
  let old = target("offline-route");
  let next = target("preferred-route");
  let control = Control::default();
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, old.clone(), forward(), true)
    .await
    .unwrap();
  registry
    .configure(&control, next.clone(), forward(), true)
    .await
    .unwrap();
  registry.activate(&control, &old).await;
  assert_eq!(*control.changes.lock().unwrap(), Vec::<Change>::new());
  control.ready.lock().unwrap().insert(next.clone());
  registry.activate(&control, &next).await;
  assert_eq!(
    control.changes.lock().unwrap().as_slice(),
    &[Change {
      target: next.clone(),
      forward: forward(),
      cancel: false
    },]
  );
  assert_eq!(
    registry.list(&control, &next).await[0].state,
    PortForwardState::Active
  );
}

#[tokio::test]
async fn configure_during_master_startup_is_not_activated_twice() {
  let target = target("connecting");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  // A replacement master clears old listeners. Another client then configures
  // this forward while ensure_master waits for its credential save offer.
  registry.master_replaced(&target);
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  registry.activate(&control, &target).await;
  assert_eq!(control.changes.lock().unwrap().len(), 2);
  assert_eq!(
    registry.list(&control, &target).await[0].state,
    PortForwardState::Active
  );
  assert!(registry.records[&forward().forward_id].listener_present);
}

#[tokio::test]
async fn temporary_unready_status_does_not_erase_listener_cancellation_responsibility() {
  let old = target("temporarily-unavailable");
  let next = target("preferred");
  let control = Control::ready(&[old.clone(), next.clone()]);
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, old.clone(), forward(), true)
    .await
    .unwrap();
  control.ready.lock().unwrap().remove(&old);
  assert_eq!(
    registry.list(&control, &old).await[0].state,
    PortForwardState::WaitingForAuthentication
  );
  control.fail_cancel.store(true, Ordering::SeqCst);
  assert!(
    registry
      .configure(&control, next.clone(), forward(), true)
      .await
      .is_err()
  );
  assert!(registry.records[&forward().forward_id].listener_present);
  assert_eq!(registry.records[&forward().forward_id].target, old);
  control.fail_cancel.store(false, Ordering::SeqCst);
  registry
    .configure(&control, next, forward(), true)
    .await
    .unwrap();
  assert_eq!(
    control.changes.lock().unwrap()[2],
    Change {
      target: old,
      forward: forward(),
      cancel: true,
    }
  );
}

#[tokio::test]
async fn disconnect_retains_definitions_and_suppresses_replay_until_explicit_resume() {
  let target = target("paused");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  registry.pause(&target);
  registry.master_replaced(&target);
  // A stale master check may still succeed while disconnect completes.
  registry.activate(&control, &target).await;
  let status = registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  assert_eq!(status.state, PortForwardState::WaitingForAuthentication);
  assert_eq!(registry.list(&control, &target).await, vec![status]);
  assert_eq!(registry.records.len(), 1);
  assert_eq!(control.changes.lock().unwrap().len(), 1);

  registry.resume(&target);
  registry.activate(&control, &target).await;
  assert_eq!(
    registry.list(&control, &target).await[0].state,
    PortForwardState::Active
  );
  assert_eq!(control.changes.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn failed_disconnect_retains_listener_cancellation_responsibility() {
  let target = target("exit-failed");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  registry.pause(&target);
  assert!(registry.records[&forward().forward_id].listener_present);
  control.fail_cancel.store(true, Ordering::SeqCst);
  assert!(
    registry
      .configure(&control, target.clone(), forward(), false)
      .await
      .is_err()
  );
  registry.activate(&control, &target).await;
  assert_eq!(control.changes.lock().unwrap().len(), 2);
  assert!(registry.records[&forward().forward_id].listener_present);
  control.fail_cancel.store(false, Ordering::SeqCst);
  registry
    .configure(&control, target, forward(), false)
    .await
    .unwrap();
  assert_eq!(
    registry.records.keys().collect::<Vec<_>>(),
    Vec::<&String>::new()
  );
}

#[tokio::test]
async fn shared_disconnect_cancels_owned_listeners_and_reconnect_replays_the_definition() {
  let target = target("shared-master");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  registry.disconnect(&control, &target).await.unwrap();
  assert!(!registry.records[&forward().forward_id].listener_present);
  assert_eq!(registry.records.len(), 1);
  assert_eq!(
    control.changes.lock().unwrap()[1],
    Change {
      target: target.clone(),
      forward: forward(),
      cancel: true,
    }
  );
  registry.activate(&control, &target).await;
  assert_eq!(control.changes.lock().unwrap().len(), 2);
  registry.resume(&target);
  registry.activate(&control, &target).await;
  assert_eq!(
    control.changes.lock().unwrap()[2],
    Change {
      target: target.clone(),
      forward: forward(),
      cancel: false,
    }
  );
  assert_eq!(
    registry.list(&control, &target).await[0].state,
    PortForwardState::Active
  );
}

#[tokio::test]
async fn shared_disconnect_retains_failed_listener_cancellation_for_retry() {
  let target = target("shared-master");
  let control = Control::ready(std::slice::from_ref(&target));
  let mut registry = ForwardRegistry::default();
  registry
    .configure(&control, target.clone(), forward(), true)
    .await
    .unwrap();
  control.fail_cancel.store(true, Ordering::SeqCst);
  assert!(registry.disconnect(&control, &target).await.is_err());
  assert!(registry.records[&forward().forward_id].listener_present);
  assert!(registry.paused.contains(&target));
  control.fail_cancel.store(false, Ordering::SeqCst);
  registry.disconnect(&control, &target).await.unwrap();
  assert!(!registry.records[&forward().forward_id].listener_present);
}
