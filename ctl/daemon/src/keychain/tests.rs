use super::*;
use ctl_ipc::{GatewayKind, SshGateway, SshGatewayMode, VpnGateway};
use std::cell::RefCell;
use std::collections::HashSet;

fn vpn_target() -> SshTarget {
  let target = SshTarget {
    destination: "alice@host.test".into(),
    ssh_config_alias: None,
    use_ssh_config_master: Some(false),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![SshGateway {
      kind: GatewayKind::Vpn,
      vpn: Some(VpnGateway {
        connection_id: "work".into(),
        socket_path: "/tmp/desktop-ctld.sock".into(),
        expected_remote_id: None,
      }),
      destination: "work".into(),
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      mode: SshGatewayMode::Automatic,
    }],
  };
  assert!(ctl_ipc::has_valid_gateway_route(&target.gateways));
  target
}

#[test]
fn shared_password_takes_precedence_without_reading_legacy_secret() {
  let target = vpn_target();
  let stable = scope_id(&target);
  let mut reads = Vec::new();
  let result = lookup_scopes(&target, |scope| {
    reads.push(scope.to_owned());
    assert_eq!(scope, stable);
    Ok::<_, ()>(Some("shared fixture"))
  });
  assert_eq!(result, Ok(Some("shared fixture")));
  assert_eq!(reads, [stable]);
}

#[test]
fn missing_shared_password_checks_only_the_exact_legacy_scope() {
  let target = vpn_target();
  let scopes = lookup_scope_ids(&target);
  assert_eq!(scopes.len(), 2);
  let mut reads = Vec::new();
  let result = lookup_scopes(&target, |scope| {
    reads.push(scope.to_owned());
    Ok::<_, ()>((scope == scopes[1]).then_some("legacy fixture"))
  });
  assert_eq!(result, Ok(Some("legacy fixture")));
  assert_eq!(reads, scopes);
}

#[test]
fn failed_shared_password_read_never_falls_back_or_prompts_again() {
  let target = vpn_target();
  let mut reads = 0;
  let result = lookup_scopes(&target, |_| {
    reads += 1;
    Err::<Option<()>, _>("authorization denied")
  });
  assert_eq!(result, Err("authorization denied"));
  assert_eq!(reads, 1);
}

#[test]
fn absent_credentials_remain_missing_after_both_exact_lookups() {
  let target = vpn_target();
  let mut reads = Vec::new();
  let result = lookup_scopes(&target, |scope| {
    reads.push(scope.to_owned());
    Ok::<Option<()>, ()>(None)
  });
  assert_eq!(result, Ok(None));
  assert_eq!(reads, lookup_scope_ids(&target));
}

#[test]
fn never_save_is_honored_in_either_exact_scope() {
  let target = vpn_target();
  let scopes = lookup_scope_ids(&target);
  for never_scope in &scopes {
    let never = lookup_scopes(&target, |scope| {
      Ok::<_, ()>((scope == never_scope).then_some(()))
    });
    assert_eq!(never, Ok(Some(())));
  }
  let never = lookup_scopes(&target, |_| Ok::<Option<()>, ()>(None));
  assert_eq!(never, Ok(None));
}

#[test]
fn ordinary_ssh_has_one_lookup_and_no_duplicate_authorization() {
  let mut target = vpn_target();
  target.gateways.clear();
  let mut reads = 0;
  let result = lookup_scopes(&target, |_| {
    reads += 1;
    Ok::<Option<()>, ()>(None)
  });
  assert_eq!(result, Ok(None));
  assert_eq!(reads, 1);
}

#[test]
fn replacing_then_forgetting_a_password_cannot_restore_its_exact_legacy_copy() {
  let target = vpn_target();
  let scopes = lookup_scope_ids(&target);
  let prompt = digest(b"alice@host.test's password:");
  let other_prompt = digest(b"verification code:");
  let store = RefCell::new(HashMap::from([
    ((scopes[1].clone(), prompt.clone()), "old fixture"),
    (
      (scopes[1].clone(), other_prompt.clone()),
      "unrelated fixture",
    ),
  ]));
  let metadata = RefCell::new(store.borrow().keys().cloned().collect::<HashSet<_>>());
  replace_scopes(
    &target,
    |scope| {
      let id = (scope.to_owned(), prompt.clone());
      store.borrow_mut().insert(id.clone(), "replacement fixture");
      metadata.borrow_mut().insert(id);
      Ok::<_, ()>(())
    },
    |scope| {
      let id = (scope.to_owned(), prompt.clone());
      store.borrow_mut().remove(&id);
      metadata.borrow_mut().remove(&id);
      Ok(())
    },
  )
  .unwrap();
  let mut cli = target.clone();
  cli.gateways[0].vpn.as_mut().unwrap().socket_path = "/tmp/cli-ctld.sock".into();
  assert_eq!(
    lookup_scopes(&cli, |scope| {
      Ok::<_, ()>(
        store
          .borrow()
          .get(&(scope.to_owned(), prompt.clone()))
          .copied(),
      )
    }),
    Ok(Some("replacement fixture"))
  );
  let shared_id = (scopes[0].clone(), prompt.clone());
  store.borrow_mut().remove(&shared_id);
  metadata.borrow_mut().remove(&shared_id);
  assert_eq!(
    lookup_scopes(&target, |scope| {
      Ok::<_, ()>(
        store
          .borrow()
          .get(&(scope.to_owned(), prompt.clone()))
          .copied(),
      )
    }),
    Ok(None)
  );
  let unrelated_id = (scopes[1].clone(), other_prompt);
  assert_eq!(
    store.borrow().get(&unrelated_id),
    Some(&"unrelated fixture")
  );
  assert_eq!(*metadata.borrow(), HashSet::from([unrelated_id]));
}

#[test]
fn failed_replacement_preserves_the_saved_legacy_password() {
  let target = vpn_target();
  let store = RefCell::new(HashMap::from([(
    lookup_scope_ids(&target)[1].clone(),
    "legacy fixture",
  )]));
  let result = replace_scopes(
    &target,
    |_| Err("save failed"),
    |scope| {
      store.borrow_mut().remove(scope);
      Ok(())
    },
  );
  assert_eq!(result, Err("save failed"));
  assert_eq!(
    lookup_scopes(&target, |scope| Ok::<_, ()>(
      store.borrow().get(scope).copied()
    )),
    Ok(Some("legacy fixture"))
  );
}

#[test]
fn host_cleanup_removes_both_exact_scopes_and_preserves_other_routes() {
  let target = vpn_target();
  let scopes = lookup_scope_ids(&target);
  let mut other = target.clone();
  other.destination = "other-host.test".into();
  let other_scope = scope_id(&other);
  let mut records = HashMap::new();
  let mut metadata = HashSet::new();
  let mut policies = HashSet::new();
  for scope in [&scopes[0], &scopes[1], &other_scope] {
    policies.insert(scope.clone());
    for prompt in ["password", "verification"] {
      let id = (scope.clone(), digest(prompt.as_bytes()));
      records.insert(id.clone(), "fixture");
      metadata.insert(id);
    }
  }
  remove_scopes(&target, |scope| {
    records.retain(|(item_scope, _), _| item_scope != scope);
    metadata.retain(|(item_scope, _)| item_scope != scope);
    policies.remove(scope);
    Ok::<_, ()>(())
  })
  .unwrap();
  assert_eq!(records.len(), 2);
  assert_eq!(metadata.len(), 2);
  assert!(records.keys().all(|(scope, _)| scope == &other_scope));
  assert!(metadata.iter().all(|(scope, _)| scope == &other_scope));
  assert_eq!(policies, HashSet::from([other_scope]));
}
