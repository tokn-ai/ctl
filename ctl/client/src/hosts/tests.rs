use super::*;
use serde_json::json;

fn catalog() -> HostCatalogDocument {
  serde_json::from_value(json!({
    "schema_version": 1,
    "hosts": [{"host_id": "host-1", "name": "work", "preferred_method_id": "office", "connection_methods": [
      {"method_id": "office", "name": "VPN", "target": {"kind": "ssh", "destination": "10.0.0.20", "user": "alice", "vpn_connection_id": "office-vpn", "gateway_route": [{"gateway_id": "proxy", "mode": "automatic"}]}},
      {"method_id": "lan", "name": "Local network", "ssh_config_alias": "lan-box", "target": {"kind": "ssh", "destination": "lan-box"}}
    ]}],
    "ssh_gateways": [{"gateway_id": "proxy", "name": "Proxy", "kind": "socks5", "destination": "127.0.0.1", "port": 1080}]
  })).unwrap()
}

#[test]
fn saved_names_ids_methods_and_routes_share_one_resolution() {
  let catalog = catalog();
  catalog.validate().unwrap();
  for name in ["work", "host-1"] {
    let resolved = resolve(&catalog, name, None).unwrap();
    let target = resolved.target.to_ssh_target().unwrap();
    assert_eq!(target.destination, "10.0.0.20");
    assert_eq!(target.user.as_deref(), Some("alice"));
    assert_eq!(
      target.gateways[0].vpn.as_ref().unwrap().connection_id,
      "office-vpn"
    );
    assert_eq!(target.gateways[1].kind, ctl_ipc::GatewayKind::Socks5);
    assert!(!target.uses_ssh_config_master());
  }
  let target = resolve(&catalog, "work", Some("Local network"))
    .unwrap()
    .target
    .to_ssh_target()
    .unwrap();
  assert_eq!(target.destination, "lan-box");
  assert!(target.uses_ssh_config_master());
  assert_eq!(target.gateways, Vec::<ctl_ipc::SshGateway>::new());
}

#[test]
fn explicit_user_overrides_the_saved_account() {
  let resolved = resolve(&catalog(), "bob@work", None).unwrap();
  assert_eq!(
    resolved.target.to_ssh_target().unwrap().user.as_deref(),
    Some("bob")
  );
}

#[test]
fn unknown_names_fall_back_but_ambiguous_saved_names_fail() {
  let mut catalog = catalog();
  let unknown = resolve(&catalog, "alice@other-host", None).unwrap();
  assert!(unknown.host_id.is_none());
  assert_eq!(unknown.target.label(), "alice@other-host");
  assert!(resolve(&catalog, "other-host", Some("office")).is_err());
  let mut duplicate = catalog.hosts[0].clone();
  duplicate.host_id = "host-2".into();
  catalog.hosts.push(duplicate);
  assert_eq!(
    resolve(&catalog, "work", None).err().unwrap().code,
    "host_ambiguous"
  );
  assert!(resolve(&catalog, "host-1", None).is_ok());
}

#[test]
fn invalid_saved_settings_cannot_be_silently_dropped() {
  let mut value = serde_json::to_value(catalog()).unwrap();
  value["hosts"][0]["connection_methods"][0]["target"]["proxy_command"] = json!("unknown setting");
  assert!(serde_json::from_value::<HostCatalogDocument>(value).is_err());
}

#[test]
fn tailscale_bindings_follow_device_identity_and_reject_a_missing_device() {
  let mut catalog = catalog();
  catalog.hosts[0].connection_methods[0].tailscale_node_id = Some("node-1".into());
  let mut resolved = resolve(&catalog, "work", None).unwrap();
  assert!(resolved.resolve_tailscale(&[]).is_err());
  resolved
    .resolve_tailscale(&[crate::tailscale::TailscaleDevice {
      node_id: "node-1".into(),
      name: "Renamed machine".into(),
      dns_name: Some("new-name.example.ts.net".into()),
      addresses: vec!["100.64.0.9".into()],
      online: Some(true),
      os: None,
    }])
    .unwrap();
  let target = resolved.target.to_ssh_target().unwrap();
  assert_eq!(target.destination, "new-name.example.ts.net");
  assert_eq!(target.hostname.as_deref(), Some("100.64.0.9"));
}

#[test]
fn stable_ids_win_over_display_names_and_literal_names_can_include_at_signs() {
  let mut catalog = catalog();
  let mut second = catalog.hosts[0].clone();
  second.host_id = "host-2".into();
  second.name = "host-1".into();
  catalog.hosts.push(second);
  assert_eq!(
    resolve(&catalog, "host-1", None)
      .unwrap()
      .host_id
      .as_deref(),
    Some("host-1")
  );
  catalog.hosts[0].name = "work@office".into();
  assert_eq!(
    resolve(&catalog, "work@office", None)
      .unwrap()
      .host_id
      .as_deref(),
    Some("host-1")
  );
}

fn mixed_catalog(route: &serde_json::Value) -> HostCatalogDocument {
  serde_json::from_value(json!({
    "schema_version": 1,
    "hosts": [{
      "host_id": "target", "name": "Target", "preferred_method_id": "mixed",
      "connection_methods": [{"method_id": "mixed", "name": "Mixed", "target": {
        "kind": "ssh", "destination": "target.internal", "gateway_route": route
      }}]
    }],
    "ssh_gateways": [
      {"gateway_id": "a", "name": "A", "destination": "jump-a", "user": "alice",
        "remote_info": {"remote_id": "11111111-1111-4111-8111-111111111111", "agent_version": "0.1.0"}},
      {"gateway_id": "b", "name": "B", "destination": "jump-b", "port": 2222,
        "remote_info": {"remote_id": "22222222-2222-4222-8222-222222222222", "agent_version": "0.1.0"}},
      {"gateway_id": "proxy", "name": "Proxy", "kind": "socks5", "destination": "proxy.internal", "port": 1080}
    ]
  })).unwrap()
}

#[test]
fn ordered_vpns_follow_the_exact_ssh_execution_prefix_and_identity() {
  let catalog = mixed_catalog(&json!([
    {"vpn_connection_id": "local-vpn"},
    {"gateway_id": "a", "mode": "automatic"},
    {"vpn_connection_id": "shared-profile"},
    {"gateway_id": "b", "mode": "automatic"},
    {"vpn_connection_id": "shared-profile"}
  ]));
  catalog.validate().unwrap();
  let target = resolve(&catalog, "target", None).unwrap().target;
  let route = target.vpn_route().unwrap();
  assert_eq!(route.len(), 3);
  assert_eq!(route[0].connection_id, "local-vpn");
  assert!(route[0].owner.is_none());
  let first = route[1].owner.as_ref().unwrap();
  assert_eq!(first.destination, "jump-a");
  assert_eq!(first.user.as_deref(), Some("alice"));
  assert_eq!(first.gateways.len(), 1);
  assert_eq!(
    first.gateways[0].vpn.as_ref().unwrap().connection_id,
    "local-vpn"
  );
  assert_eq!(
    route[1].expected_remote_id.as_deref(),
    Some("11111111-1111-4111-8111-111111111111")
  );
  let second = route[2].owner.as_ref().unwrap();
  assert_eq!(second.destination, "jump-b");
  assert_eq!(second.port, Some(2222));
  assert_eq!(second.gateways.len(), 3);
  assert_eq!(
    second.gateways[2].vpn.as_ref().unwrap().expected_remote_id,
    route[1].expected_remote_id
  );
  assert_eq!(
    route[2].expected_remote_id.as_deref(),
    Some("22222222-2222-4222-8222-222222222222")
  );
  assert!(!target.to_ssh_target().unwrap().uses_ssh_config_master());
}

#[test]
fn vpn_nesting_and_proxy_execution_are_rejected_without_changing_legacy_routes() {
  for route in [
    json!([{"vpn_connection_id": "a"}, {"vpn_connection_id": "b"}]),
    json!([{"gateway_id": "proxy", "mode": "automatic"}, {"vpn_connection_id": "b"}]),
  ] {
    assert!(mixed_catalog(&route).validate().is_err());
  }
  catalog().validate().unwrap();
  // An ambiguous step cannot silently drop either half of the route.
  assert!(
    serde_json::from_value::<SshGatewayRouteStepDto>(json!({
      "gateway_id": "a", "mode": "automatic", "vpn_connection_id": "b"
    }))
    .is_err()
  );
}

#[test]
fn remote_vpn_runtime_dtos_reject_stale_endpoints_and_invalid_owner_pins() {
  let catalog = mixed_catalog(&json!([
    {"gateway_id": "a", "mode": "automatic"}, {"vpn_connection_id": "vpn"}
  ]));
  let mut target = resolve(&catalog, "target", None).unwrap().target;
  let ConnectionTargetDto::Ssh { gateways, .. } = &mut target else {
    panic!("SSH target")
  };
  gateways[1].port = Some(1080);
  assert!(target.to_ssh_target().is_err());
  let ConnectionTargetDto::Ssh { gateways, .. } = &mut target else {
    panic!("SSH target")
  };
  gateways[1].port = None;
  gateways[0].remote_info.as_mut().unwrap().remote_id =
    "00000000-0000-0000-0000-000000000000".into();
  assert!(target.to_ssh_target().is_err());
}
