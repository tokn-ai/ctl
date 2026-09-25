use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use super::repository::Repository;
use super::*;
use crate::dto::{ConnectionTargetDto, SshGatewayModeDto};

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("rmux-workspace-test-{}", uuid::Uuid::new_v4())))
  }

  fn repository(&self) -> Repository {
    Repository::new(self.0.clone())
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ignored = fs::remove_dir_all(&self.0);
  }
}

fn legacy_populated() -> WorkspaceDocument {
  let mut document = WorkspaceDocument {
    schema_version: 7,
    ..WorkspaceDocument::default()
  };
  document.hosts.push(WorkspaceHost {
    host_id: "local".into(),
    name: "Local".into(),
    connection_methods: Vec::new(),
    preferred_method_id: None,
    remote_info: None,
  });
  document.hosts.push(WorkspaceHost {
    host_id: "remote-id".into(),
    name: "test".into(),
    connection_methods: vec![WorkspaceConnectionMethod {
      method_id: "default".into(),
      name: "SSH".into(),
      ssh_config_alias: None,
      use_ssh_config_master: None,
      tailscale_node_id: None,
      target: ConnectionTargetDto::ssh("test"),
    }],
    preferred_method_id: Some("default".into()),
    remote_info: None,
  });
  document.sessions.push(WorkspaceSession {
    host_id: "remote-id".into(),
    session_id: "session-id".into(),
    name: "shell".into(),
    last_known_cwd: Some("/work".into()),
    last_known_cwd_display: Some("~/work".into()),
  });
  document.tabs.push(document.sessions[0].reference().into());
  document.active_tab = document.tabs.first().cloned();
  document
}

fn populated() -> WorkspaceDocument {
  let mut document = legacy_populated();
  document.schema_version = 8;
  document.hosts.clear();
  document
}

#[test]
fn missing_workspace_loads_empty_and_round_trips_only_metadata() {
  let fixture = Fixture::new();
  let initial = fixture.repository().load().unwrap();
  assert_eq!(initial, WorkspaceSnapshot::default());
  assert!(!fixture.0.join("workspace.json").exists());
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), saved);
  let bytes = fs::read_to_string(fixture.0.join("workspace.json")).unwrap();
  for runtime_field in [
    "password",
    "attachment_token",
    "next_sequence",
    "running_command",
    "terminal_size",
  ] {
    assert!(!bytes.contains(runtime_field));
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
      fs::metadata(fixture.0.join("workspace.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }
}

#[test]
fn corrupt_and_future_workspaces_are_never_overwritten() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let mut future = WorkspaceSnapshot {
    revision: Some("future".into()),
    document: populated(),
  };
  future.document.schema_version = 99;
  for bytes in [
    b"broken json".to_vec(),
    serde_json::to_vec(&future).unwrap(),
  ] {
    fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
    assert!(fixture.repository().load().is_err());
    assert!(
      fixture
        .repository()
        .update(UpdateWorkspaceRequest {
          expected_revision: None,
          document: populated()
        })
        .is_err()
    );
    assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
  }
}

#[test]
fn stale_and_concurrent_writers_cannot_lose_updates() {
  let fixture = Fixture::new();
  let gate = Arc::new(Barrier::new(3));
  let threads: Vec<_> = (0..2)
    .map(|_| {
      let repository = fixture.repository();
      let gate = Arc::clone(&gate);
      std::thread::spawn(move || {
        gate.wait();
        repository.update(UpdateWorkspaceRequest {
          expected_revision: None,
          document: populated(),
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
    "workspace_conflict"
  );
  let saved = fixture.repository().load().unwrap();
  assert_eq!(saved.document, populated());
}

#[test]
fn validates_membership_tabs_and_host_identity() {
  let mut document = populated();
  document.sessions[0].host_id = String::new();
  assert!(document.validate().is_err());
  document = populated();
  document.tabs.push(document.tabs[0].clone());
  assert!(document.validate().is_err());
  document = populated();
  document.active_tab = Some(WorkspaceTab::Session {
    host_id: "remote-id".into(),
    session_id: "absent".into(),
  });
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts.push(document.hosts[1].clone());
  assert!(document.validate().is_err());
}

#[test]
fn failed_write_preserves_previous_document() {
  let fixture = Fixture::new();
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated(),
    })
    .unwrap();
  let mut invalid = populated();
  invalid.sessions[0].last_known_cwd = Some("x".repeat(5000));
  assert!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: saved.revision.clone(),
        document: invalid
      })
      .is_err()
  );
  assert_eq!(fixture.repository().load().unwrap(), saved);
}

#[cfg(unix)]
#[test]
fn refuses_symlinks_without_modifying_their_target() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let target = fixture.0.join("original");
  fs::write(&target, "preserve").unwrap();
  std::os::unix::fs::symlink(&target, fixture.0.join("workspace.json")).unwrap();
  assert!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: None,
        document: populated()
      })
      .is_err()
  );
  assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
}

#[test]
fn migrates_legacy_tabs_without_losing_order_and_preserves_a_backup() {
  let fixture = Fixture::new();
  fixture.repository().load().unwrap();
  let mut value = serde_json::to_value(WorkspaceSnapshot {
    revision: Some("old".into()),
    document: legacy_populated(),
  })
  .unwrap();
  value["document"]["schema_version"] = 1.into();
  for tab in value["document"]["tabs"].as_array_mut().unwrap() {
    tab.as_object_mut().unwrap().remove("kind");
  }
  value["document"]["active_tab"]
    .as_object_mut()
    .unwrap()
    .remove("kind");
  value["document"]
    .as_object_mut()
    .unwrap()
    .remove("task_definitions");
  value["document"]
    .as_object_mut()
    .unwrap()
    .remove("task_references");
  let bytes = serde_json::to_vec(&value).unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  let loaded = fixture.repository().load().unwrap();
  assert_eq!(loaded.document, populated());
  assert_ne!(loaded.revision.as_deref(), Some("old"));
  assert_eq!(
    fs::read(fixture.0.join("workspace-v1.backup.json")).unwrap(),
    bytes
  );
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: loaded.revision,
      document: loaded.document,
    })
    .unwrap();
  assert_eq!(
    fixture.repository().load().unwrap().document.schema_version,
    8
  );
}

#[test]
fn migrates_v3_workspace_to_current_schema() {
  let fixture = Fixture::new();
  fs::create_dir_all(&fixture.0).unwrap();
  let mut document = legacy_populated();
  document.schema_version = 3;
  let bytes = serde_json::to_vec(&WorkspaceSnapshot {
    revision: Some("before-port-forwarding".into()),
    document,
  })
  .unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();

  let loaded = fixture.repository().load().unwrap();

  assert_eq!(loaded.document.schema_version, 8);
  assert!(loaded.document.port_forwards.is_empty());
  assert_eq!(
    fs::read(fixture.0.join("workspace-v3.backup.json")).unwrap(),
    bytes
  );
}

#[test]
fn migrates_v4_workspace_to_sidebar_schema() {
  let fixture = Fixture::new();
  fs::create_dir_all(&fixture.0).unwrap();
  let mut document = legacy_populated();
  document.schema_version = 4;
  document.port_forwards.push(WorkspacePortForward {
    forward_id: uuid::Uuid::new_v4().to_string(),
    host_id: "remote-id".into(),
    name: "Database".into(),
    bind_address: "127.0.0.1".into(),
    local_port: 5432,
    remote_host: "127.0.0.1".into(),
    remote_port: 5432,
    enabled: false,
  });
  let bytes = serde_json::to_vec(&WorkspaceSnapshot {
    revision: Some("before-ports-sidebar".into()),
    document,
  })
  .unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();

  let loaded = fixture.repository().load().unwrap();

  assert_eq!(loaded.document.schema_version, 8);
  assert_eq!(loaded.document.sidebar_view, SidebarView::Sessions);
  assert_eq!(loaded.document.port_forwards.len(), 1);
  assert_eq!(
    fs::read(fixture.0.join("workspace-v4.backup.json")).unwrap(),
    bytes
  );
}

#[test]
fn migrates_v5_workspace_to_gateway_schema_and_preserves_a_backup() {
  let fixture = Fixture::new();
  fs::create_dir_all(&fixture.0).unwrap();
  let mut document = legacy_populated();
  document.schema_version = 5;
  let bytes = serde_json::to_vec(&WorkspaceSnapshot {
    revision: Some("before-gateway-routes".into()),
    document,
  })
  .unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();

  let loaded = fixture.repository().load().unwrap();

  assert_eq!(loaded.document.schema_version, 8);
  assert!(loaded.document.ssh_gateways.is_empty());
  assert_eq!(
    fs::read(fixture.0.join("workspace-v5.backup.json")).unwrap(),
    bytes
  );
}

#[test]
fn gateway_routes_require_unique_saved_gateway_references() {
  let mut document = legacy_populated();
  document.ssh_gateways.push(WorkspaceSshGateway {
    gateway_id: "edge".into(),
    name: "Edge".into(),
    destination: "edge.example".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    remote_info: None,
  });
  if let ConnectionTargetDto::Ssh { gateway_route, .. } =
    &mut document.hosts[1].connection_methods[0].target
  {
    gateway_route.push(crate::dto::SshGatewayRouteStepDto {
      gateway_id: "edge".into(),
      mode: SshGatewayModeDto::Automatic,
    });
  } else {
    panic!("expected SSH target");
  }
  assert!(document.validate().is_ok());

  if let ConnectionTargetDto::Ssh { gateway_route, .. } =
    &mut document.hosts[1].connection_methods[0].target
  {
    gateway_route.push(gateway_route[0].clone());
  }
  assert!(document.validate().is_err());
  if let ConnectionTargetDto::Ssh { gateway_route, .. } =
    &mut document.hosts[1].connection_methods[0].target
  {
    gateway_route.pop();
    gateway_route[0].gateway_id = "missing".into();
  }
  assert!(document.validate().is_err());
}

#[test]
fn port_forwards_require_a_remote_workspace_host_and_loopback_binding() {
  let mut document = populated();
  document.port_forwards.push(WorkspacePortForward {
    forward_id: uuid::Uuid::new_v4().to_string(),
    host_id: "remote-id".into(),
    name: "Database".into(),
    bind_address: "127.0.0.1".into(),
    local_port: 5432,
    remote_host: "127.0.0.1".into(),
    remote_port: 5432,
    enabled: true,
  });
  assert!(document.validate().is_ok());

  document.port_forwards[0].bind_address = "0.0.0.0".into();
  assert!(document.validate().is_err());
  document.port_forwards[0].bind_address = "127.0.0.1".into();
  document.port_forwards[0].host_id = "local".into();
  assert!(document.validate().is_err());
}

#[test]
fn incomplete_task_drafts_round_trip_without_becoming_runnable_definitions() {
  let fixture = Fixture::new();
  let mut document = populated();
  document.sidebar_view = SidebarView::Tasks;
  document.task_drafts.push(TaskDefinitionDraft {
    command_line: Some("cargo run \"unfinished".into()),
    scope: None,
    base_revision: DraftBaseRevision::Unknown,
    definition_id: uuid::Uuid::new_v4().to_string(),
    definition: task_proto::TaskDefinition {
      name: String::new(),
      program: String::new(),
      arguments: vec![String::new()],
      working_directory: Some("unfinished/relative".into()),
      execution_mode: task_proto::ExecutionMode::Background,
    },
  });
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: document.clone(),
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap().document, document);
  document.task_drafts.push(document.task_drafts[0].clone());
  assert!(document.validate().is_err());
}

fn legacy_definition() -> SavedTaskDefinition {
  SavedTaskDefinition {
    definition_id: uuid::Uuid::new_v4().to_string(),
    revision: "legacy-revision".into(),
    definition: task_proto::TaskDefinition {
      name: "build".into(),
      program: "cargo".into(),
      arguments: vec!["build".into()],
      working_directory: None,
      execution_mode: task_proto::ExecutionMode::Background,
    },
  }
}

fn write_legacy(fixture: &Fixture, saved: &SavedTaskDefinition) -> Vec<u8> {
  fs::create_dir_all(&fixture.0).unwrap();
  let mut document = legacy_populated();
  document.schema_version = 2;
  document.task_definitions.push(saved.clone());
  document.task_references.push(TaskReference {
    host_id: "local".into(),
    task_id: uuid::Uuid::new_v4().to_string(),
    definition_id: Some(saved.definition_id.clone()),
    definition_scope: None,
    applied_revision: Some(saved.revision.clone()),
    is_default: true,
  });
  let bytes = serde_json::to_vec(&WorkspaceSnapshot {
    revision: Some("workspace-before-import".into()),
    document,
  })
  .unwrap();
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  bytes
}

#[test]
fn imports_definitions_once_and_preserves_refs_and_legacy_directory_semantics() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let original = write_legacy(&fixture, &saved);
  let store = task_store::Repository::new(fixture.0.join("tasks.json"));
  // Simulate a crash after import but before the workspace migration commits.
  store.import_legacy(std::slice::from_ref(&saved)).unwrap();
  let snapshot = fixture.repository().load().unwrap();
  assert_eq!(snapshot.document.schema_version, 8);
  assert!(snapshot.document.task_definitions.is_empty());
  let definitions = store.load().unwrap().definitions;
  assert_eq!(definitions.len(), 1);
  assert_eq!(definitions[0].definition, saved.definition);
  assert_eq!(definitions[0].definition_id, saved.definition_id);
  let original: WorkspaceSnapshot = serde_json::from_slice(&original).unwrap();
  assert_eq!(snapshot.document.sessions, original.document.sessions);
  assert_eq!(
    snapshot.document.task_references[0].task_id,
    original.document.task_references[0].task_id
  );
  assert_eq!(
    snapshot.document.task_references[0]
      .applied_revision
      .as_deref(),
    Some(definitions[0].revision.as_str())
  );
  assert!(fixture.0.join("workspace-v2.backup.json").is_file());
  assert_eq!(fixture.repository().load().unwrap(), snapshot);

  // A later CLI change cannot be overwritten by persisting an old workspace view.
  let mut updated = saved.definition;
  updated.arguments = vec!["test".into()];
  store
    .save(
      &saved.definition_id,
      Some(&definitions[0].revision),
      updated.clone(),
    )
    .unwrap();
  fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: snapshot.revision,
      document: snapshot.document,
    })
    .unwrap();
  assert_eq!(store.load().unwrap().definitions[0].definition, updated);
}

#[test]
fn conflicting_import_keeps_workspace_and_shared_definition_unchanged() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let bytes = write_legacy(&fixture, &saved);
  let store = task_store::Repository::new(fixture.0.join("tasks.json"));
  let mut other = saved.definition.clone();
  other.arguments = vec!["test".into()];
  store
    .save(&saved.definition_id, None, other.clone())
    .unwrap();
  assert!(fixture.repository().load().is_err());
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
  assert_eq!(store.load().unwrap().definitions[0].definition, other);
}

#[test]
fn incomplete_migration_backup_does_not_authorize_replacing_the_workspace() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let original = write_legacy(&fixture, &saved);
  let backup = fixture.0.join("workspace-v2.backup.json");
  fs::write(&backup, b"partial").unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "workspace_backup_conflict"
  );
  assert_eq!(
    fs::read(fixture.0.join("workspace.json")).unwrap(),
    original
  );
  assert_eq!(fs::read(backup).unwrap(), b"partial");
  assert!(!fixture.0.join("tasks.json").exists());
}

#[test]
fn migration_that_exceeds_the_size_limit_preserves_the_readable_source() {
  let fixture = Fixture::new();
  let saved = legacy_definition();
  let bytes = write_legacy(&fixture, &saved);
  let mut original: WorkspaceSnapshot = serde_json::from_slice(&bytes).unwrap();
  let reference = original.document.task_references[0].clone();
  for _ in 0..14_000 {
    original.document.task_references.push(TaskReference {
      task_id: uuid::Uuid::new_v4().to_string(),
      is_default: false,
      ..reference.clone()
    });
  }
  original.document.validate().unwrap();
  let bytes = serde_json::to_vec(&original).unwrap();
  assert!(bytes.len() < 4 * 1024 * 1024);
  fs::write(fixture.0.join("workspace.json"), &bytes).unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "workspace_too_large"
  );
  assert_eq!(fs::read(fixture.0.join("workspace.json")).unwrap(), bytes);
}

#[test]
fn draft_revision_distinguishes_unknown_base_from_new_definition() {
  let mut value = serde_json::json!({
    "definition_id": "draft",
    "definition": legacy_definition().definition,
  });
  let old: TaskDefinitionDraft = serde_json::from_value(value.clone()).unwrap();
  assert_eq!(old.base_revision, DraftBaseRevision::Unknown);
  value["base_revision"] = serde_json::Value::Null;
  let new: TaskDefinitionDraft = serde_json::from_value(value.clone()).unwrap();
  assert_eq!(new.base_revision, DraftBaseRevision::New);
  assert!(serde_json::to_value(new).unwrap()["base_revision"].is_null());
  value["base_revision"] = "saved-revision".into();
  let edited: TaskDefinitionDraft = serde_json::from_value(value).unwrap();
  assert_eq!(
    edited.base_revision,
    DraftBaseRevision::Saved("saved-revision".into())
  );
}

#[test]
fn remote_metadata_round_trips_and_rejects_corruption_without_changing_saved_sessions() {
  let fixture = Fixture::new();
  let identity = ctl_proto::RemoteIdentity {
    remote_id: uuid::Uuid::new_v4().to_string(),
    agent_version: "0.1.0".into(),
    rmux_restart_supported: false,
    bundle: Some(Box::new(ctl_proto::BundleVersion {
      app_version: "0.1.0".into(),
      bundle_id: "development-abc".into(),
      git_revision: "abc".into(),
      target_triple: "x86_64-unknown-linux-musl".into(),
    })),
  };
  let mut document = populated();
  document.host_identities.push(WorkspaceHostIdentity {
    host_id: "remote-id".into(),
    remote_info: identity,
  });
  let saved = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document,
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), saved);
  let mut invalid = saved.document.clone();
  invalid.host_identities[0].remote_info.remote_id = "bad-id".into();
  assert!(
    fixture
      .repository()
      .update(UpdateWorkspaceRequest {
        expected_revision: saved.revision.clone(),
        document: invalid
      })
      .is_err()
  );
  assert_eq!(fixture.repository().load().unwrap(), saved);
}

#[test]
fn named_hosts_and_methods_round_trip_without_rebinding_owned_references() {
  let fixture = Fixture::new();
  let workspace = fixture
    .repository()
    .update(UpdateWorkspaceRequest {
      expected_revision: None,
      document: populated(),
    })
    .unwrap();
  let mut document = HostCatalogDocument {
    hosts: legacy_populated()
      .hosts
      .into_iter()
      .filter(|host| host.host_id != "local")
      .collect(),
    ..HostCatalogDocument::default()
  };
  let host = &mut document.hosts[0];
  host.name = "Development machine".into();
  let mut alternate = host.connection_methods[0].clone();
  alternate.method_id = "vpn".into();
  alternate.name = "Through the office VPN".into();
  host.connection_methods.push(alternate);
  host.preferred_method_id = Some("vpn".into());
  // Addresses belong to methods and are not globally unique host identifiers.
  let mut other = host.clone();
  other.host_id = "another-machine".into();
  document.hosts.push(other);
  let saved = fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: None,
      document: document.clone(),
    })
    .unwrap();
  assert_eq!(
    fixture.repository().load_hosts().unwrap().document,
    document
  );
  document.hosts[0].name = "Renamed machine".into();
  document.hosts[0].preferred_method_id = Some("default".into());
  fixture
    .repository()
    .update_hosts(UpdateHostsRequest {
      expected_revision: saved.revision,
      document,
    })
    .unwrap();
  assert_eq!(fixture.repository().load().unwrap(), workspace);
}

#[test]
fn host_methods_require_a_valid_preference_and_unique_ids() {
  let mut document = legacy_populated();
  document.hosts[1].preferred_method_id = None;
  assert!(document.validate().is_err());
  document.hosts[1].preferred_method_id = Some("missing".into());
  assert!(document.validate().is_err());
  document = legacy_populated();
  let duplicate = document.hosts[1].connection_methods[0].clone();
  document.hosts[1].connection_methods.push(duplicate);
  assert!(document.validate().is_err());
  document.hosts[1].connection_methods.clear();
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts[1].connection_methods[0].target = ConnectionTargetDto::Local;
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts[1].name = "bad\nname".into();
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts[1].connection_methods[0].name = String::new();
  assert!(document.validate().is_err());
}

#[test]
fn local_host_cannot_have_methods_preferences_or_remote_identity() {
  let mut document = legacy_populated();
  let method = document.hosts[1].connection_methods[0].clone();
  document.hosts[0].connection_methods.push(method);
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts[0].preferred_method_id = Some("default".into());
  assert!(document.validate().is_err());
  document = legacy_populated();
  document.hosts[0].remote_info = Some(test_remote_identity());
  assert!(document.validate().is_err());
}

fn test_remote_identity() -> ctl_proto::RemoteIdentity {
  ctl_proto::RemoteIdentity {
    remote_id: uuid::Uuid::new_v4().to_string(),
    agent_version: "0.1.0".into(),
    rmux_restart_supported: false,
    bundle: None,
  }
}

#[test]
fn methods_cannot_override_the_hosts_verified_environment() {
  let mut document = legacy_populated();
  if let ConnectionTargetDto::Ssh { remote_info, .. } =
    &mut document.hosts[1].connection_methods[0].target
  {
    *remote_info = Some(Box::new(test_remote_identity()));
  }
  assert!(document.validate().is_err());
}

fn legacy_host_document(schema_version: u32) -> serde_json::Value {
  let mut value = serde_json::to_value(WorkspaceSnapshot {
    revision: Some("before-host-methods".into()),
    document: legacy_populated(),
  })
  .unwrap();
  value["document"]["schema_version"] = schema_version.into();
  value["document"]["hosts"] = serde_json::json!([
    { "host_id": "local", "target": { "kind": "local" } },
    { "host_id": "remote-id", "target": { "kind": "ssh", "destination": "test" } }
  ]);
  if schema_version == 1 {
    for tab in value["document"]["tabs"].as_array_mut().unwrap() {
      tab.as_object_mut().unwrap().remove("kind");
    }
    value["document"]["active_tab"]
      .as_object_mut()
      .unwrap()
      .remove("kind");
  }
  value
}

#[test]
fn all_legacy_host_schemas_migrate_without_changing_host_or_session_ids() {
  for schema_version in 1..=6 {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.0).unwrap();
    let value = legacy_host_document(schema_version);
    let original = serde_json::to_vec(&value).unwrap();
    fs::write(fixture.0.join("workspace.json"), &original).unwrap();
    let loaded = fixture.repository().load().unwrap();
    assert_eq!(loaded.document, populated());
    assert_ne!(loaded.revision.as_deref(), Some("before-host-methods"));
    assert_eq!(
      fs::read(
        fixture
          .0
          .join(format!("workspace-v{schema_version}.backup.json"))
      )
      .unwrap(),
      original
    );
    assert_eq!(fixture.repository().load().unwrap(), loaded);
  }
}

#[test]
fn v6_migration_preserves_identity_gateway_routes_and_port_ownership() {
  let fixture = Fixture::new();
  fs::create_dir_all(&fixture.0).unwrap();
  let mut value = legacy_host_document(6);
  let identity = test_remote_identity();
  value["document"]["hosts"][1]["target"]["remote_info"] = serde_json::to_value(&identity).unwrap();
  value["document"]["hosts"][1]["target"]["gateway_route"] = serde_json::json!([
    { "gateway_id": "edge", "mode": "automatic" }
  ]);
  value["document"]["ssh_gateways"] = serde_json::json!([
    { "gateway_id": "edge", "name": "Office gateway", "destination": "edge.example" }
  ]);
  value["document"]["port_forwards"] = serde_json::json!([{
    "forward_id": "db", "host_id": "remote-id", "name": "Database",
    "bind_address": "127.0.0.1", "local_port": 5432,
    "remote_host": "127.0.0.1", "remote_port": 5432, "enabled": true
  }]);
  let original = serde_json::to_vec(&value).unwrap();
  fs::write(fixture.0.join("workspace.json"), &original).unwrap();
  let loaded = fixture.repository().load().unwrap();
  let catalog = fixture.repository().load_hosts().unwrap();
  let host = &catalog.document.hosts[0];
  assert_eq!(host.remote_info.as_ref(), Some(&identity));
  assert_eq!(host.preferred_method_id.as_deref(), Some("default"));
  let ConnectionTargetDto::Ssh {
    remote_info,
    gateway_route,
    ..
  } = &host.connection_methods[0].target
  else {
    panic!("expected migrated SSH method");
  };
  assert!(remote_info.is_none());
  assert_eq!(gateway_route[0].gateway_id, "edge");
  assert_eq!(loaded.document.port_forwards[0].host_id, "remote-id");
  assert_eq!(loaded.document.sessions, populated().sessions);
  assert_eq!(loaded.document.tabs, populated().tabs);
  assert_eq!(
    fs::read(fixture.0.join("workspace-v6.backup.json")).unwrap(),
    original
  );
}

#[test]
fn conflicting_v6_backup_preserves_the_original_document() {
  let fixture = Fixture::new();
  fs::create_dir_all(&fixture.0).unwrap();
  let original = serde_json::to_vec(&legacy_host_document(6)).unwrap();
  fs::write(fixture.0.join("workspace.json"), &original).unwrap();
  fs::write(
    fixture.0.join("workspace-v6.backup.json"),
    b"other workspace",
  )
  .unwrap();
  assert_eq!(
    fixture.repository().load().unwrap_err().code,
    "workspace_backup_conflict"
  );
  assert_eq!(
    fs::read(fixture.0.join("workspace.json")).unwrap(),
    original
  );
}
