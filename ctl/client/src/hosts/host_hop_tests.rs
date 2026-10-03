use super::*;
use serde_json::{Value, json};

fn host(id: &str, route: Value) -> Value {
  let mut host = json!({
    "host_id": id, "name": id, "preferred_method_id": "selected",
    "connection_methods": [{
      "method_id": "selected", "name": "Selected", "target": {
        "kind": "ssh", "destination": format!("{id}.internal")
      }
    }]
  });
  host["connection_methods"][0]["target"]["gateway_route"] = route;
  host
}

fn linked(id: &str) -> Value {
  json!({"host_id": id, "method_id": "selected", "mode": "automatic"})
}

fn catalog(hosts: Vec<Value>) -> HostCatalogDocument {
  let mut catalog = json!({
    "schema_version": 1, "hosts": [], "ssh_gateways": [{
      "gateway_id": "outer", "name": "Outer", "destination": "outer.internal"
    }]
  });
  catalog["hosts"] = Value::Array(hosts);
  serde_json::from_value(catalog).unwrap()
}

fn runtime(catalog: &HostCatalogDocument) -> ConnectionTargetDto {
  resolve(catalog, "target", None).unwrap().target
}

#[test]
fn linked_methods_expand_their_current_routes_and_stay_selected() {
  let mut jump = host(
    "jump",
    json!([{"gateway_id": "outer", "mode": "native_only"}]),
  );
  jump["connection_methods"][0]["target"]["user"] = json!("alice");
  jump["connection_methods"][0]["target"]["port"] = json!(2222);
  jump["remote_info"] = json!({
    "remote_id": "11111111-1111-4111-8111-111111111111", "agent_version": "0.1.0"
  });
  let mut catalog = catalog(vec![
    host(
      "target",
      json!([linked("jump"), {"vpn_connection_id": "work"}]),
    ),
    jump,
  ]);
  catalog.validate().unwrap();
  let target = runtime(&catalog);
  let gateways = target.ssh_gateways();
  assert_eq!(
    gateways
      .iter()
      .map(|gateway| gateway.destination.as_str())
      .collect::<Vec<_>>(),
    ["outer.internal", "jump.internal", "work"]
  );
  assert_eq!(gateways[0].mode, ctl_ipc::SshGatewayMode::NativeOnly);
  assert_eq!(gateways[1].user.as_deref(), Some("alice"));
  assert_eq!(gateways[1].port, Some(2222));
  let route = target.vpn_route().unwrap();
  assert_eq!(
    route[0].expected_remote_id.as_deref(),
    Some("11111111-1111-4111-8111-111111111111")
  );
  assert_eq!(route[0].owner.as_ref().unwrap().gateways.len(), 1);

  let method = &mut catalog.hosts[1].connection_methods[0];
  if let ConnectionTargetDto::Ssh { destination, .. } = &mut method.target {
    *destination = "renamed.internal".into();
  }
  let mut alternate = method.clone();
  alternate.method_id = "alternate".into();
  alternate.target = ConnectionTargetDto::ssh("other.internal");
  catalog.hosts[1].connection_methods.push(alternate);
  catalog.hosts[1].preferred_method_id = Some("alternate".into());
  assert_eq!(
    runtime(&catalog).ssh_gateways()[1].destination,
    "renamed.internal"
  );
}

#[test]
fn nested_routes_are_inserted_without_deduplicating_prefixes() {
  let catalog = catalog(vec![
    host(
      "target",
      json!([
        {"gateway_id": "outer", "mode": "automatic"}, linked("jump")
      ]),
    ),
    host(
      "jump",
      json!([{ "gateway_id": "outer", "mode": "automatic" }]),
    ),
  ]);
  catalog.validate().unwrap();
  let gateways = runtime(&catalog).ssh_gateways();
  assert_eq!(gateways.len(), 3);
  assert_eq!(gateways[0].destination, gateways[1].destination);
}

#[test]
fn legacy_vpn_in_a_linked_method_uses_the_preceding_inserted_host() {
  let mut jump = host("jump", json!([]));
  jump["connection_methods"][0]["target"]["vpn_connection_id"] = json!("work");
  let catalog = catalog(vec![
    host(
      "target",
      json!([{"gateway_id": "outer", "mode": "automatic"}, linked("jump")]),
    ),
    jump,
  ]);
  catalog.validate().unwrap();
  let routes = runtime(&catalog).vpn_route().unwrap();
  assert_eq!(
    routes[0].owner.as_ref().unwrap().destination,
    "outer.internal"
  );
}

#[test]
fn missing_linked_hosts_and_methods_prevent_catalog_mutation() {
  let mut catalog = catalog(vec![
    host("target", json!([linked("jump")])),
    host("jump", json!([])),
  ]);
  catalog.hosts[1].connection_methods[0].method_id = "replacement".into();
  catalog.hosts[1].preferred_method_id = Some("replacement".into());
  assert_eq!(
    catalog.validate().unwrap_err().code,
    "host_hop_method_missing"
  );
  catalog.hosts.pop();
  assert_eq!(catalog.validate().unwrap_err().code, "host_hop_missing");
  assert_eq!(
    resolve(&catalog, "target", None).err().unwrap().code,
    "host_hop_missing"
  );
}

#[test]
fn route_cycles_include_the_destination_method() {
  for catalog in [
    catalog(vec![host("target", json!([linked("target")]))]),
    catalog(vec![
      host("target", json!([linked("jump")])),
      host("jump", json!([linked("target")])),
    ]),
  ] {
    assert_eq!(catalog.validate().unwrap_err().code, "host_route_cycle");
    assert_eq!(
      resolve(&catalog, "target", None).err().unwrap().code,
      "host_route_cycle"
    );
  }
  let mut target = host(
    "target",
    json!([{
      "host_id": "target", "method_id": "alternate", "mode": "automatic"
    }]),
  );
  target["connection_methods"].as_array_mut().unwrap().push(json!({
    "method_id": "alternate", "name": "Alternate", "target": { "kind": "ssh", "destination": "other.internal" }
  }));
  assert_eq!(
    catalog(vec![target]).validate().unwrap_err().code,
    "host_route_cycle"
  );
}

#[test]
fn the_hop_limit_counts_the_expanded_route() {
  let chain = |count: usize| {
    let mut hosts = vec![host("target", json!([linked("jump-0")]))];
    for index in 0..count {
      let route = if index + 1 == count {
        json!([])
      } else {
        json!([linked(&format!("jump-{}", index + 1))])
      };
      hosts.push(host(&format!("jump-{index}"), route));
    }
    catalog(hosts)
  };
  let valid = chain(8);
  valid.validate().unwrap();
  assert_eq!(runtime(&valid).ssh_gateways().len(), 8);
  assert_eq!(chain(9).validate().unwrap_err().code, "host_route_too_long");
  assert_eq!(
    chain(100).validate().unwrap_err().code,
    "host_route_too_long"
  );
}

#[test]
fn vpn_adjacency_is_checked_after_linked_method_expansion() {
  let mut jump = host("jump", json!([{ "vpn_connection_id": "inner" }]));
  let mut catalog = catalog(vec![
    host(
      "target",
      json!([{ "vpn_connection_id": "outer" }, linked("jump")]),
    ),
    jump.clone(),
  ]);
  assert_eq!(catalog.validate().unwrap_err().code, "invalid_vpn_route");
  jump["connection_methods"][0]["target"]["gateway_route"] = json!([]);
  catalog.hosts[1] = serde_json::from_value(jump).unwrap();
  catalog.validate().unwrap();
}

#[test]
fn unsupported_hop_key_settings_are_rejected_without_dropping_the_key() {
  let mut jump = host("jump", json!([]));
  jump["connection_methods"][0]["target"]["identity_file"] = json!("~/.ssh/special");
  let catalog = catalog(vec![host("target", json!([linked("jump")])), jump]);
  let error = catalog.validate().unwrap_err();
  assert_eq!(error.code, "host_hop_identity_unsupported");
  assert!(error.message.contains("OpenSSH"));
}

#[test]
fn embedded_accounts_are_normalized_and_explicit_accounts_win() {
  let mut jump = host("jump", json!([]));
  jump["connection_methods"][0]["target"]["destination"] = json!("embedded@jump.internal");
  let mut catalog = catalog(vec![host("target", json!([linked("jump")])), jump]);
  let gateway = runtime(&catalog).ssh_gateways().remove(0);
  assert_eq!(gateway.destination, "jump.internal");
  assert_eq!(gateway.user.as_deref(), Some("embedded"));
  let ConnectionTargetDto::Ssh { user, .. } = &mut catalog.hosts[1].connection_methods[0].target
  else {
    panic!("SSH")
  };
  *user = Some("explicit".into());
  assert_eq!(
    runtime(&catalog).ssh_gateways()[0].user.as_deref(),
    Some("explicit")
  );
}

#[test]
fn alias_overrides_survive_remote_vpn_owner_preparation() {
  let mut jump = host("jump", json!([]));
  jump["connection_methods"][0]["ssh_config_alias"] = json!("jump.internal");
  jump["connection_methods"][0]["target"]["hostname"] = json!("10.0.0.7");
  let catalog = catalog(vec![
    host(
      "target",
      json!([linked("jump"), { "vpn_connection_id": "work" }]),
    ),
    jump,
  ]);
  catalog.validate().unwrap();
  let target = runtime(&catalog);
  let owner = target.vpn_route().unwrap().remove(0).owner.unwrap();
  assert_eq!(owner.destination, "jump.internal");
  assert_eq!(owner.ssh_config_alias.as_deref(), Some("jump.internal"));
  assert_eq!(owner.hostname.as_deref(), Some("10.0.0.7"));
  assert!(!owner.uses_ssh_config_master());
}

#[test]
fn tailscale_refresh_includes_nested_hops_and_is_atomic() {
  let mut jump = host("jump", json!([linked("inner")]));
  jump["connection_methods"][0]["tailscale_node_id"] = json!("jump-node");
  let mut inner = host("inner", json!([]));
  inner["connection_methods"][0]["tailscale_node_id"] = json!("inner-node");
  let catalog = catalog(vec![host("target", json!([linked("jump")])), jump, inner]);
  catalog.validate().unwrap();
  let mut resolved = resolve(&catalog, "target", None).unwrap();
  assert!(resolved.requires_tailscale());
  assert!(resolved.tailscale_node_id.is_none());
  let device = |id: &str, address: &str| crate::tailscale::TailscaleDevice {
    node_id: id.into(),
    name: id.into(),
    dns_name: Some(format!("{id}.ts.net")),
    addresses: vec![address.into()],
    online: Some(true),
    os: None,
  };
  let original = resolved.target.clone();
  assert!(
    resolved
      .resolve_tailscale(&[device("inner-node", "100.64.0.1")])
      .is_err()
  );
  assert_eq!(resolved.target, original);
  resolved
    .resolve_tailscale(&[
      device("inner-node", "100.64.0.1"),
      device("jump-node", "100.64.0.2"),
    ])
    .unwrap();
  let gateways = resolved.target.ssh_gateways();
  assert_eq!(gateways[0].destination, "inner-node.ts.net");
  assert_eq!(gateways[0].hostname.as_deref(), Some("100.64.0.1"));
  assert_eq!(gateways[1].destination, "jump-node.ts.net");
  assert_eq!(gateways[1].hostname.as_deref(), Some("100.64.0.2"));
}

#[test]
fn ambiguous_host_route_shapes_are_rejected() {
  for step in [
    json!({ "host_id": "jump", "method_id": "selected", "mode": "automatic", "gateway_id": "outer" }),
    json!({ "host_id": "jump", "method_id": "selected", "mode": "automatic", "vpn_connection_id": "work" }),
    json!({ "host_id": "jump", "mode": "automatic" }),
  ] {
    assert!(serde_json::from_value::<SshGatewayRouteStepDto>(step).is_err());
  }
}
