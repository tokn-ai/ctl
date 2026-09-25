use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::repository::Repository;
use super::*;
use crate::dto::{ConnectionTargetDto, SshGatewayModeDto, SshGatewayRouteStepDto};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("rmux-host-catalog-test-{}", uuid::Uuid::new_v4())))
  }

  fn repository(&self) -> Repository {
    Repository::new(self.0.clone())
  }

  fn write_legacy(&self, document: WorkspaceDocument) -> Vec<u8> {
    fs::create_dir_all(&self.0).unwrap();
    let mut bytes = serde_json::to_vec(&WorkspaceSnapshot {
      revision: Some("legacy".into()),
      document,
    })
    .unwrap();
    bytes.extend_from_slice(b"\n\n");
    fs::write(self.0.join("workspace.json"), &bytes).unwrap();
    bytes
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

fn host() -> WorkspaceHost {
  WorkspaceHost {
    host_id: "ssh-config:office".into(),
    name: "Office machine".into(),
    connection_methods: vec![WorkspaceConnectionMethod {
      method_id: "default".into(),
      name: "SSH".into(),
      ssh_config_alias: None,
      use_ssh_config_master: None,
      tailscale_node_id: None,
      target: ConnectionTargetDto::ssh("office"),
    }],
    preferred_method_id: Some("default".into()),
    remote_info: Some(identity()),
  }
}

fn identity() -> ctl_proto::RemoteIdentity {
  ctl_proto::RemoteIdentity {
    remote_id: "9dcefd7e-2b35-43d8-97d9-7508186dbac0".into(),
    agent_version: "0.1.0".into(),
    rmux_restart_supported: false,
    bundle: None,
  }
}

fn catalog() -> HostCatalogDocument {
  HostCatalogDocument {
    hosts: vec![host()],
    ..HostCatalogDocument::default()
  }
}

#[test]
fn tailscale_method_binding_round_trips_without_becoming_a_transport_setting() {
  let fixture = Fixture::new();
  let mut document = catalog();
  document.hosts[0].connection_methods[0].tailscale_node_id = Some("n-stable-device".into());
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document,
    })
    .unwrap();
  assert_eq!(fixture.repository().load_hosts().unwrap(), saved);
  let value: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.0.join("hosts.json")).unwrap()).unwrap();
  let method = &value["document"]["hosts"][0]["connection_methods"][0];
  assert_eq!(method["tailscale_node_id"], "n-stable-device");
  assert!(method["target"].get("tailscale_node_id").is_none());
}

#[test]
fn ssh_config_origin_round_trips_at_the_method_level() {
  let fixture = Fixture::new();
  let mut document = catalog();
  document.hosts[0].connection_methods[0].ssh_config_alias = Some("office".into());
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document,
    })
    .unwrap();
  assert_eq!(fixture.repository().load_hosts().unwrap(), saved);
  let value: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.0.join("hosts.json")).unwrap()).unwrap();
  let method = &value["document"]["hosts"][0]["connection_methods"][0];
  assert_eq!(method["ssh_config_alias"], "office");
  assert!(method["target"].get("ssh_config_alias").is_none());
}

#[test]
fn explicit_ssh_master_policy_round_trips_at_the_method_level() {
  for ssh_config_alias in [None, Some("office".to_owned())] {
    for use_ssh_config_master in [false, true] {
      let fixture = Fixture::new();
      let mut document = catalog();
      let method = &mut document.hosts[0].connection_methods[0];
      method.ssh_config_alias.clone_from(&ssh_config_alias);
      method.use_ssh_config_master = Some(use_ssh_config_master);
      let saved = fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document,
        })
        .unwrap();
      assert_eq!(fixture.repository().load_hosts().unwrap(), saved);
      let value: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("hosts.json")).unwrap()).unwrap();
      let method = &value["document"]["hosts"][0]["connection_methods"][0];
      assert_eq!(method["use_ssh_config_master"], use_ssh_config_master);
      assert!(method["target"].get("use_ssh_config_master").is_none());
    }
  }
}

#[test]
fn legacy_methods_keep_the_source_default_without_saving_an_explicit_policy() {
  for ssh_config_alias in [None, Some("office".to_owned())] {
    let mut host = host();
    host.connection_methods[0].ssh_config_alias = ssh_config_alias;
    let value = serde_json::to_value(&host).unwrap();
    assert!(
      value["connection_methods"][0]
        .get("use_ssh_config_master")
        .is_none()
    );
    let restored: WorkspaceHost = serde_json::from_value(value).unwrap();
    assert_eq!(restored, host);
    assert_eq!(restored.connection_methods[0].use_ssh_config_master, None);
  }
}

#[test]
fn persisted_targets_reject_runtime_ssh_master_policy() {
  for use_ssh_config_master in [false, true] {
    let mut value = serde_json::to_value(host()).unwrap();
    value["connection_methods"][0]["target"]["use_ssh_config_master"] =
      use_ssh_config_master.into();
    assert!(serde_json::from_value::<WorkspaceHost>(value).is_err());

    let fixture = Fixture::new();
    let mut document = catalog();
    let ConnectionTargetDto::Ssh {
      use_ssh_config_master: target_policy,
      ..
    } = &mut document.hosts[0].connection_methods[0].target
    else {
      panic!("ssh method")
    };
    *target_policy = Some(use_ssh_config_master);
    assert!(
      fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document,
        })
        .is_err()
    );
    assert!(!fixture.0.join("hosts.json").exists());
  }
}

#[test]
fn invalid_ssh_config_origins_and_runtime_target_origins_are_rejected() {
  for alias in [
    "",
    "office\nother",
    "office other",
    "!office",
    "-office",
    "*",
    "host?",
  ] {
    let fixture = Fixture::new();
    let mut document = catalog();
    document.hosts[0].connection_methods[0].ssh_config_alias = Some(alias.into());
    assert!(
      fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document,
        })
        .is_err(),
      "accepted {alias:?}",
    );
    assert!(!fixture.0.join("hosts.json").exists());
  }

  let mut value = serde_json::to_value(host()).unwrap();
  value["connection_methods"][0]["target"]["ssh_config_alias"] = "office".into();
  assert!(serde_json::from_value::<WorkspaceHost>(value).is_err());

  let fixture = Fixture::new();
  let mut document = catalog();
  let ConnectionTargetDto::Ssh {
    ssh_config_alias, ..
  } = &mut document.hosts[0].connection_methods[0].target
  else {
    panic!("ssh method")
  };
  *ssh_config_alias = Some("office".into());
  assert!(
    fixture
      .repository()
      .update_hosts(UpdateHostsRequest {
        expected_revision: None,
        document,
      })
      .is_err()
  );
}

#[test]
fn ssh_config_methods_require_one_provider_and_the_original_destination() {
  for (alias, tailscale_node_id) in [
    ("another-host", None),
    ("office", Some("n-stable-device".to_owned())),
  ] {
    let fixture = Fixture::new();
    let mut document = catalog();
    let method = &mut document.hosts[0].connection_methods[0];
    method.ssh_config_alias = Some(alias.into());
    method.tailscale_node_id = tailscale_node_id;
    assert!(
      fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document,
        })
        .is_err()
    );
    assert!(!fixture.0.join("hosts.json").exists());
  }
}

#[test]
fn legacy_methods_need_no_tailscale_binding_and_invalid_bindings_are_rejected() {
  let value = serde_json::to_value(host()).unwrap();
  assert!(
    value["connection_methods"][0]
      .get("tailscale_node_id")
      .is_none()
  );
  let restored: WorkspaceHost = serde_json::from_value(value).unwrap();
  assert_eq!(restored.connection_methods[0].tailscale_node_id, None);
  for node_id in ["", "n\nbad", "n bad", "../device"] {
    let fixture = Fixture::new();
    let mut document = catalog();
    document.hosts[0].connection_methods[0].tailscale_node_id = Some(node_id.into());
    assert!(
      fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document,
        })
        .is_err(),
      "accepted {node_id:?}",
    );
    assert!(!fixture.0.join("hosts.json").exists());
  }
}

fn legacy() -> WorkspaceDocument {
  let mut document = WorkspaceDocument {
    schema_version: 7,
    ..WorkspaceDocument::default()
  };
  document.hosts = vec![
    WorkspaceHost {
      host_id: "local".into(),
      name: "Local".into(),
      connection_methods: Vec::new(),
      preferred_method_id: None,
      remote_info: None,
    },
    host(),
  ];
  document.sessions.push(WorkspaceSession {
    host_id: host().host_id,
    session_id: "remembered".into(),
    name: "Shell".into(),
    last_known_cwd: None,
    last_known_cwd_display: None,
  });
  document.tabs.push(document.sessions[0].reference().into());
  document.active_tab = document.tabs.first().cloned();
  document
}

#[test]
fn saved_hosts_are_separate_from_workspace_state_and_local_is_implicit() {
  let fixture = Fixture::new();
  assert_eq!(
    fixture.repository().load_hosts().unwrap(),
    HostCatalogSnapshot::default()
  );
  assert!(!fixture.0.join("hosts.json").exists());
  assert!(!fixture.0.join("workspace.json").exists());
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document: catalog(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load_hosts().unwrap(), saved);
  assert!(!fixture.0.join("workspace.json").exists());
  let workspace = WorkspaceDocument {
    sidebar_view: SidebarView::Ports,
    ..WorkspaceDocument::default()
  };
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: workspace,
    })
    .unwrap();
  let value: serde_json::Value =
    serde_json::from_slice(&fs::read(fixture.0.join("workspace.json")).unwrap()).unwrap();
  assert!(value["document"].get("hosts").is_none());
  assert!(value["document"].get("ssh_gateways").is_none());
  assert_eq!(fixture.repository().load_hosts().unwrap(), saved);
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
      fs::metadata(fixture.0.join("hosts.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }
}

#[test]
fn concurrent_catalog_writers_do_not_lose_updates() {
  let fixture = Fixture::new();
  let gate = Arc::new(Barrier::new(3));
  let threads: Vec<_> = (0..2)
    .map(|_| {
      let repository = fixture.repository();
      let gate = Arc::clone(&gate);
      std::thread::spawn(move || {
        gate.wait();
        repository.update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document: catalog(),
        })
      })
    })
    .collect();
  gate.wait();
  let results: Vec<_> = threads
    .into_iter()
    .map(|thread| thread.join().unwrap())
    .collect();
  assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
  assert_eq!(
    results
      .iter()
      .find_map(|result| result.as_ref().err())
      .unwrap()
      .code,
    "hosts_conflict"
  );
}

#[test]
fn content_revisions_detect_external_edits_without_revision_updates() {
  let fixture = Fixture::new();
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document: catalog(),
    })
    .unwrap();
  let mut changed = saved.clone();
  changed.document.hosts[0].name = "Edited outside rmux".into();
  let bytes = serde_json::to_vec(&changed).unwrap();
  fs::write(fixture.0.join("hosts.json"), &bytes).unwrap();
  assert_eq!(
    fixture
      .repository()
      .update_hosts(UpdateHostsRequest {
        expected_revision: saved.revision,
        document: saved.document
      })
      .unwrap_err()
      .code,
    "hosts_conflict"
  );
  assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), bytes);
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: WorkspaceDocument::default(),
    })
    .unwrap();
  let mut changed = saved.clone();
  changed.document.sidebar_view = SidebarView::Tasks;
  let bytes = serde_json::to_vec(&changed).unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  assert_eq!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: saved.revision,
        document: saved.document
      })
      .unwrap_err()
      .code,
    "workspace_conflict"
  );
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
}

#[test]
fn catalog_migration_retries_after_catalog_commit_and_keeps_original_backup() {
  let fixture = Fixture::new();
  // Model a crash after the first commit; host commands normally migrate first.
  fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document: catalog(),
    })
    .unwrap();
  let old = legacy();
  let bytes = fixture.write_legacy(old.clone());
  let loaded_catalog = fixture.repository().load_hosts().unwrap();
  let loaded = fixture.repository().load().unwrap();
  assert_eq!(loaded_catalog.document, catalog());
  assert_eq!(loaded.document.schema_version, 8);
  assert!(loaded.document.hosts.is_empty());
  assert_eq!(loaded.document.sessions, old.sessions);
  assert_eq!(loaded.document.tabs, old.tabs);
  assert_eq!(
    loaded.document.host_identities,
    vec![WorkspaceHostIdentity {
      host_id: host().host_id,
      remote_info: identity()
    }]
  );
  assert_eq!(
    fs::read(fixture.0.join("workspace-v7.backup.json")).unwrap(),
    bytes
  );
  assert_eq!(fixture.repository().load().unwrap(), loaded);
}

#[test]
fn conflicting_migration_does_not_overwrite_either_catalog_or_workspace() {
  let fixture = Fixture::new();
  let mut other = catalog();
  other.hosts[0].name = "Separately saved".into();
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document: other,
    })
    .unwrap();
  let old = fixture.write_legacy(legacy());
  let catalog_bytes = fs::read(fixture.0.join("hosts.json")).unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "hosts_import_conflict"
  );
  assert_eq!(
    fixture.repository().load_hosts().unwrap_err().code,
    "hosts_import_conflict"
  );
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), old);
  assert_eq!(
    fs::read(fixture.0.join("hosts.json")).unwrap(),
    catalog_bytes
  );
  assert!(
    fixture
      .repository()
      .update_hosts(UpdateHostsRequest {
        expected_revision: saved.revision,
        document: catalog()
      })
      .is_err()
  );
}

#[test]
fn host_and_gateway_import_is_one_validated_batch() {
  let fixture = Fixture::new();
  let mut old = legacy();
  let gateway = WorkspaceSshGateway {
    gateway_id: "edge".into(),
    name: "Office gateway".into(),
    destination: "edge".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    remote_info: None,
  };
  old.ssh_gateways.push(gateway.clone());
  let ConnectionTargetDto::Ssh { gateway_route, .. } =
    &mut old.hosts[1].connection_methods[0].target
  else {
    panic!("ssh method")
  };
  gateway_route.push(SshGatewayRouteStepDto {
    gateway_id: gateway.gateway_id.clone(),
    mode: SshGatewayModeDto::Automatic,
  });
  fixture.write_legacy(old.clone());
  let saved = fixture.repository().load_hosts().unwrap();
  assert_eq!(saved.document.ssh_gateways, vec![gateway]);
  assert_eq!(saved.document.hosts, vec![old.hosts[1].clone()]);
  assert!(
    fixture
      .repository()
      .load()
      .unwrap()
      .document
      .ssh_gateways
      .is_empty()
  );
}

#[test]
fn corrupt_future_or_unknown_catalog_data_is_preserved() {
  let fixture = Fixture::new();
  fixture.repository().load_hosts().unwrap();
  let good = serde_json::to_value(HostCatalogSnapshot {
    revision: Some("saved".into()),
    document: catalog(),
  })
  .unwrap();
  let mut future = good.clone();
  future["document"]["schema_version"] = 99.into();
  let mut unknown_target = good.clone();
  unknown_target["document"]["hosts"][0]["connection_methods"][0]["target"]["new_setting"] =
    true.into();
  let mut unknown = good;
  unknown["document"]["hosts"][0]["new_setting"] = true.into();
  for bytes in [
    b"bad json".to_vec(),
    serde_json::to_vec(&future).unwrap(),
    serde_json::to_vec(&unknown).unwrap(),
    serde_json::to_vec(&unknown_target).unwrap(),
  ] {
    fs::write(fixture.0.join("hosts.json"), &bytes).unwrap();
    assert!(fixture.repository().load_hosts().is_err());
    assert!(
      fixture
        .repository()
        .update_hosts(UpdateHostsRequest {
          expected_revision: None,
          document: catalog()
        })
        .is_err()
    );
    assert_eq!(fs::read(fixture.0.join("hosts.json")).unwrap(), bytes);
  }
}

#[cfg(unix)]
#[test]
fn catalog_symlinks_are_never_followed_or_replaced() {
  let fixture = Fixture::new();
  fixture.repository().load_hosts().unwrap();
  let original = fixture.0.join("original");
  fs::write(&original, b"preserve").unwrap();
  std::os::unix::fs::symlink(&original, fixture.0.join("hosts.json")).unwrap();
  assert!(fixture.repository().load_hosts().is_err());
  assert!(
    fixture
      .repository()
      .update_hosts(UpdateHostsRequest {
        expected_revision: None,
        document: catalog()
      })
      .is_err()
  );
  assert_eq!(fs::read(original).unwrap(), b"preserve");
}

#[test]
fn missing_host_references_and_identities_survive_without_persisting_definitions() {
  let fixture = Fixture::new();
  let mut document = legacy();
  document.schema_version = 8;
  document.hosts.clear();
  document.host_identities.push(WorkspaceHostIdentity {
    host_id: host().host_id,
    remote_info: identity(),
  });
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: document.clone(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), saved);
  assert!(!fixture.0.join("hosts.json").exists());
  document
    .host_identities
    .push(document.host_identities[0].clone());
  assert!(document.validate().is_err());
  document.host_identities.pop();
  document.host_identities[0].host_id = "not-referenced".into();
  assert!(document.validate().is_err());
  document.host_identities[0].host_id = "local".into();
  assert!(document.validate().is_err());
}

#[test]
fn current_workspace_rejects_definitions_and_catalog_rejects_local_or_invalid_methods() {
  let mut document = WorkspaceDocument::default();
  document.hosts.push(host());
  assert!(document.validate().is_err());
  let mut document = catalog();
  document.hosts[0].host_id = "local".into();
  assert!(document.validate().is_err());
  document = catalog();
  document.hosts[0].preferred_method_id = Some("missing".into());
  assert!(document.validate().is_err());
  document = catalog();
  document.hosts.push(document.hosts[0].clone());
  assert!(document.validate().is_err());
}
