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
    ready(if cancel && self.fail_cancel.load(Ordering::SeqCst) {
      Err(RequestError::PortForwardFailed("cancel failed".into()))
    } else {
      Ok(())
    })
  }
}

fn target(destination: &str) -> SshTarget {
  SshTarget {
    destination: destination.into(),
    ssh_config_alias: None,
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
async fn moving_a_forward_cancels_its_exact_previous_route_before_starting_the_new_one() {
  let old = target("office-network");
  let mut next = target("preferred-route");
  next.gateways.push(ctld_ipc::SshGateway {
    destination: "bastion".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctld_ipc::SshGatewayMode::Automatic,
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
  assert!(registry.list(&control, &old).await.is_empty());
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
  assert!(registry.records.is_empty());
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
    assert!(registry.list(&control, &next).await.is_empty());
    assert_eq!(control.changes.lock().unwrap().len(), 2);
    control.fail_cancel.store(false, Ordering::SeqCst);
    registry
      .configure(&control, next.clone(), forward(), enabled)
      .await
      .unwrap();
    assert!(registry.list(&control, &old).await.is_empty());
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
  assert!(control.changes.lock().unwrap().is_empty());
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
  assert!(registry.records.is_empty());
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
