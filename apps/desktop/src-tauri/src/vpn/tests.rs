use std::fs;

use ctld_ipc::{VpnProvider, VpnSettings, VpnState};
use serde_json::json;
use zeroize::Zeroizing;

use super::models::VpnConnectionInput;
use super::*;

struct Fixture(PathBuf);

impl Fixture {
  fn new() -> Self {
    Self(std::env::temp_dir().join(format!("rmux-vpns-{}", uuid::Uuid::new_v4())))
  }

  fn repository(&self) -> Repository {
    Repository::new(self.0.clone())
  }

  fn bytes(&self) -> Vec<u8> {
    fs::read(self.0.join("vpns.json")).unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn input(password: Option<&str>) -> VpnConnectionInput {
  VpnConnectionInput {
    connection_id: "connection-one".into(),
    name: "Work VPN".into(),
    settings: VpnSettingsInput::Openconnect {
      url: "https://vpn.example.test/group".into(),
      username: "test-user".into(),
      password: password.map(|password| Zeroizing::new(password.to_owned())),
      auth_method: None,
      target_ip: None,
    },
  }
}

fn password(connection: &VpnConnection) -> &str {
  match &connection.settings {
    VpnSettings::Openconnect { password, .. } => password,
    VpnSettings::Tailscale { .. } => panic!("expected OpenConnect password"),
  }
}

fn save_request(revision: Option<String>, password: Option<&str>) -> SaveVpnConnectionRequest {
  SaveVpnConnectionRequest {
    expected_revision: revision,
    connection: input(password),
  }
}

#[test]
fn round_trip_returns_summaries_and_an_opaque_revision() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let empty = repository.load().unwrap();
  assert_eq!(empty.revision, None);
  assert!(empty.connections.is_empty());
  let password = "test '$literal' #password";
  let saved = repository.save(save_request(None, Some(password))).unwrap();
  assert_eq!(repository.load().unwrap(), saved);
  assert!(matches!(
    saved.connections[0].settings,
    models::VpnSettingsSummary::Openconnect {
      has_password: true,
      ..
    }
  ));
  let revision = saved.revision.as_ref().unwrap();
  assert!(revision.starts_with("sha256:"));
  assert_eq!(revision.len(), 71);
  let response = serde_json::to_value(&saved).unwrap();
  assert!(response["connections"][0].get("password").is_none());
  assert!(!serde_json::to_string(&saved).unwrap().contains(password));
  let stored: serde_json::Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  assert_eq!(stored["schema_version"], 2);
  assert_eq!(stored["connections"][0]["provider"], "openconnect");
  assert_eq!(stored["connections"][0]["password"], password);
  assert!(stored.get("revision").is_none());
  for field in ["endpoint", "running", "state", "container_name"] {
    assert!(stored["connections"][0].get(field).is_none());
  }
}

#[test]
fn null_password_preserves_edits_and_new_connections_require_a_password() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  assert_eq!(
    repository.save(save_request(None, None)).unwrap_err().code,
    "vpn_connections_invalid"
  );
  let saved = repository
    .save(save_request(None, Some("original-test-secret")))
    .unwrap();
  let mut edit = save_request(saved.revision, None);
  edit.connection.name = "Renamed VPN".into();
  let edited = repository.save(edit).unwrap();
  assert_eq!(edited.connections[0].name, "Renamed VPN");
  assert_eq!(
    password(&repository.connection("connection-one").unwrap()),
    "original-test-secret"
  );
  let changed = repository
    .save(save_request(
      edited.revision,
      Some("replacement-test-secret"),
    ))
    .unwrap();
  assert_eq!(
    password(&repository.connection("connection-one").unwrap()),
    "replacement-test-secret"
  );
  assert!(
    repository
      .save(save_request(changed.revision, Some("")))
      .is_err()
  );
}

#[test]
fn external_edits_and_stale_deletes_do_not_overwrite_the_file() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  let mut bytes = fixture.bytes();
  bytes.push(b'\n');
  fs::write(fixture.0.join("vpns.json"), &bytes).unwrap();
  assert_eq!(
    repository
      .save(save_request(saved.revision.clone(), None))
      .unwrap_err()
      .code,
    "vpn_connections_conflict"
  );
  assert_eq!(
    repository
      .delete(
        &DeleteVpnConnectionRequest {
          expected_revision: saved.revision,
          connection_id: "connection-one".into(),
        },
        &VpnSnapshot::default()
      )
      .unwrap_err()
      .code,
    "vpn_connections_conflict"
  );
  assert_eq!(fixture.bytes(), bytes);
}

#[test]
fn deleting_an_active_connection_is_rejected_for_each_active_state() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  for state in [VpnState::Starting, VpnState::Connected, VpnState::Stopping] {
    let status = VpnSnapshot {
      connections: vec![
        VpnStatus {
          connection_id: Some("another-connection".into()),
          state: VpnState::Connected,
          ..VpnStatus::default()
        },
        VpnStatus {
          connection_id: Some("connection-one".into()),
          state,
          ..VpnStatus::default()
        },
      ],
      supports_multiple: true,
      ..VpnSnapshot::default()
    };
    assert_eq!(
      repository
        .save_with_status(save_request(saved.revision.clone(), None), &status)
        .unwrap_err()
        .code,
      "vpn_connection_active"
    );
    assert_eq!(
      repository
        .delete(
          &DeleteVpnConnectionRequest {
            expected_revision: saved.revision.clone(),
            connection_id: "connection-one".into(),
          },
          &status
        )
        .unwrap_err()
        .code,
      "vpn_connection_active"
    );
  }
  let deleted = repository
    .delete(
      &DeleteVpnConnectionRequest {
        expected_revision: saved.revision,
        connection_id: "connection-one".into(),
      },
      &VpnSnapshot::default(),
    )
    .unwrap();
  assert!(deleted.connections.is_empty());
  assert!(repository.connection("connection-one").is_err());
}

#[test]
fn unrelated_active_vpns_do_not_block_saved_connection_changes() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  let status = VpnSnapshot {
    supports_multiple: true,
    connections: vec![VpnStatus {
      vpn_id: Some("another-connection".into()),
      connection_id: Some("another-connection".into()),
      state: VpnState::Connected,
      ..VpnStatus::default()
    }],
    ..VpnSnapshot::default()
  };
  let changed = repository
    .save_with_status(save_request(saved.revision, None), &status)
    .unwrap();
  let deleted = repository
    .delete(
      &DeleteVpnConnectionRequest {
        expected_revision: changed.revision,
        connection_id: "connection-one".into(),
      },
      &status,
    )
    .unwrap();
  assert!(deleted.connections.is_empty());
}

#[test]
fn invalid_fields_and_corrupt_json_are_rejected_without_exposing_secrets() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("valid-test-secret")))
    .unwrap();
  let original = fixture.bytes();
  for value in ["injected\nVPN_URL=bad", "carriage\rreturn", "zero\0byte"] {
    let error = repository
      .save(save_request(saved.revision.clone(), Some(value)))
      .unwrap_err();
    assert!(!error.message.contains(value));
    assert_eq!(fixture.bytes(), original);
  }
  let mut invalid_url = save_request(saved.revision.clone(), None);
  if let VpnSettingsInput::Openconnect { url, .. } = &mut invalid_url.connection.settings {
    *url = "http://vpn.example.test".into();
  }
  assert!(repository.save(invalid_url).is_err());
  let mut invalid_target = save_request(saved.revision, None);
  if let VpnSettingsInput::Openconnect { target_ip, .. } = &mut invalid_target.connection.settings {
    *target_ip = Some("target.example.test".into());
  }
  assert!(repository.save(invalid_target).is_err());
  let malformed = br#"{"schema_version":1,"connections":[{"password":"hidden-test-secret","connection_id":false}]}"#;
  fs::write(fixture.0.join("vpns.json"), malformed).unwrap();
  let error = repository.load().unwrap_err();
  assert!(!error.message.contains("hidden-test-secret"));
  assert!(
    repository
      .save(save_request(None, Some("another-test-secret")))
      .is_err()
  );
  assert_eq!(fixture.bytes(), malformed);
}

#[test]
fn unsupported_versions_duplicate_ids_and_oversized_fields_are_preserved() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  let original = fixture.bytes();
  let mut request = save_request(saved.revision, None);
  request.connection.name = "x".repeat(257);
  assert!(repository.save(request).is_err());
  assert_eq!(fixture.bytes(), original);
  let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
  document["schema_version"] = json!(3);
  fs::write(
    fixture.0.join("vpns.json"),
    serde_json::to_vec(&document).unwrap(),
  )
  .unwrap();
  assert_eq!(
    repository.load().unwrap_err().code,
    "vpn_version_unsupported"
  );
  document["schema_version"] = json!(1);
  let duplicate = document["connections"][0].clone();
  document["connections"]
    .as_array_mut()
    .unwrap()
    .push(duplicate);
  fs::write(
    fixture.0.join("vpns.json"),
    serde_json::to_vec(&document).unwrap(),
  )
  .unwrap();
  assert_eq!(
    repository.load().unwrap_err().code,
    "vpn_connections_invalid"
  );
}

#[cfg(unix)]
#[test]
fn storage_files_are_private_and_insecure_loads_are_rejected() {
  use std::os::unix::fs::PermissionsExt as _;

  let fixture = Fixture::new();
  let repository = fixture.repository();
  repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  assert_eq!(
    fs::metadata(&fixture.0).unwrap().permissions().mode() & 0o777,
    0o700
  );
  for name in ["vpns.json", "vpns.lock"] {
    assert_eq!(
      fs::metadata(fixture.0.join(name))
        .unwrap()
        .permissions()
        .mode()
        & 0o777,
      0o600
    );
  }
  fs::set_permissions(
    fixture.0.join("vpns.json"),
    fs::Permissions::from_mode(0o640),
  )
  .unwrap();
  assert_eq!(
    repository.load().unwrap_err().code,
    "vpn_storage_permissions"
  );
  assert!(repository.connection("connection-one").is_err());
  assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| {
    entry
      .unwrap()
      .file_name()
      .to_string_lossy()
      .ends_with(".tmp")
  }));
}

#[cfg(unix)]
#[test]
fn settings_and_lock_symlinks_are_never_followed() {
  use std::os::unix::fs::symlink;

  let fixture = Fixture::new();
  let repository = fixture.repository();
  repository.load().unwrap();
  let target = fixture.0.join("external-secret");
  fs::write(&target, "private-test-data").unwrap();
  symlink(&target, fixture.0.join("vpns.json")).unwrap();
  assert!(repository.load().is_err());
  assert!(
    repository
      .save(save_request(None, Some("test-secret")))
      .is_err()
  );
  fs::remove_file(fixture.0.join("vpns.json")).unwrap();
  fs::remove_file(fixture.0.join("vpns.lock")).unwrap();
  symlink(&target, fixture.0.join("vpns.lock")).unwrap();
  assert!(repository.load().is_err());
  assert_eq!(fs::read_to_string(target).unwrap(), "private-test-data");
}

#[tokio::test]
async fn native_connect_adapter_loads_the_secret_and_preserves_structured_errors() {
  let fixture = Fixture::new();
  fixture
    .repository()
    .save(save_request(None, Some("adapter-test-secret")))
    .unwrap();
  let request = ConnectVpnRequest {
    connection_id: "connection-one".into(),
  };
  let status = connect_with(fixture.0.clone(), request, |connection| async move {
    assert_eq!(password(&connection), "adapter-test-secret");
    let VpnSettings::Openconnect { url, username, .. } = connection.settings else {
      panic!("expected OpenConnect connection");
    };
    Ok(VpnStatus {
      connection_id: Some(connection.connection_id),
      vpn_url: Some(url),
      username: Some(username),
      state: VpnState::Connected,
      running: true,
      ..VpnStatus::default()
    })
  })
  .await
  .unwrap();
  assert_eq!(status.connection_id.as_deref(), Some("connection-one"));
  assert!(status.vpn_url.is_some());
  assert!(status.username.is_some());
  assert!(
    !serde_json::to_string(&status)
      .unwrap()
      .contains("adapter-test-secret")
  );
  let error = connect_with(
    fixture.0.clone(),
    ConnectVpnRequest {
      connection_id: "connection-one".into(),
    },
    |_| async {
      Err(ctld_ipc::vpn::VpnError::Daemon {
        code: "vpn_start_failed".into(),
        message: "The test gateway did not connect.".into(),
      })
    },
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "vpn_start_failed");
  assert!(!error.message.contains("adapter-test-secret"));
  let missing = connect_with(
    fixture.0.clone(),
    ConnectVpnRequest {
      connection_id: "missing".into(),
    },
    |_| async { panic!("a missing connection must not contact the daemon") },
  )
  .await
  .unwrap_err();
  assert_eq!(missing.code, "vpn_connection_not_found");
}

#[tokio::test]
async fn cancelling_a_host_wait_keeps_the_shared_vpn_start_owned() {
  let (started, ready) = tokio::sync::oneshot::channel();
  let (release, finish) = tokio::sync::oneshot::channel();
  let (completed, done) = tokio::sync::oneshot::channel();
  let waiter = tokio::spawn(await_shared_start(async move {
    started.send(()).unwrap();
    finish.await.unwrap();
    completed.send(()).unwrap();
    Ok(VpnStatus::default())
  }));
  ready.await.unwrap();
  waiter.abort();
  assert!(waiter.await.unwrap_err().is_cancelled());
  release.send(()).unwrap();
  tokio::time::timeout(std::time::Duration::from_secs(2), done)
    .await
    .unwrap()
    .unwrap();
}

#[test]
fn host_connections_require_a_ready_vpn_proxy() {
  let connected = VpnStatus {
    state: VpnState::Connected,
    running: true,
    endpoint: Some("socks5h://127.0.0.1:49152".into()),
    ..VpnStatus::default()
  };
  assert!(require_connected(&connected).is_ok());
  for status in [
    VpnStatus::default(),
    VpnStatus {
      state: VpnState::Starting,
      ..connected.clone()
    },
    VpnStatus {
      endpoint: None,
      ..connected.clone()
    },
    VpnStatus {
      running: false,
      ..connected
    },
  ] {
    assert_eq!(
      require_connected(&status).unwrap_err().code,
      "vpn_not_connected"
    );
  }
}

#[test]
fn legacy_profiles_migrate_only_when_saved_and_keep_their_password() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  repository.load().unwrap();
  let legacy = serde_json::to_vec_pretty(&json!({
    "schema_version": 1,
    "connections": [{
      "connection_id": "connection-one", "name": "Legacy VPN",
      "url": "https://vpn.example.test/group", "username": "test-user",
      "password": "legacy-test-secret", "auth_method": "password", "target_ip": null,
    }],
  }))
  .unwrap();
  let path = fixture.0.join("vpns.json");
  fs::write(&path, &legacy).unwrap();
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
  }
  let loaded = repository.load().unwrap();
  assert_eq!(fixture.bytes(), legacy);
  let connection = repository.connection("connection-one").unwrap();
  assert_eq!(connection.provider(), VpnProvider::Openconnect);
  assert_eq!(password(&connection), "legacy-test-secret");
  assert_eq!(fixture.bytes(), legacy);
  repository
    .save(save_request(loaded.revision, None))
    .unwrap();
  let saved: serde_json::Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  assert_eq!(saved["schema_version"], 2);
  assert_eq!(saved["connections"][0]["provider"], "openconnect");
  assert_eq!(saved["connections"][0]["password"], "legacy-test-secret");
}

#[test]
fn tailscale_profiles_store_settings_without_openconnect_credentials() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(SaveVpnConnectionRequest {
      expected_revision: None,
      connection: VpnConnectionInput {
        connection_id: "tailscale-one".into(),
        name: "Tailnet".into(),
        settings: VpnSettingsInput::Tailscale {
          hostname: Some("rmux-test".into()),
          accept_routes: true,
        },
      },
    })
    .unwrap();
  let summary = serde_json::to_value(&saved.connections[0]).unwrap();
  let stored: serde_json::Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  for value in [&summary, &stored["connections"][0]] {
    assert_eq!(value["provider"], "tailscale");
    assert_eq!(value["hostname"], "rmux-test");
    assert_eq!(value["accept_routes"], true);
    for field in [
      "url",
      "username",
      "password",
      "has_password",
      "auth_method",
      "target_ip",
    ] {
      assert!(value.get(field).is_none(), "unexpected field: {field}");
    }
  }
  assert_eq!(repository.load().unwrap(), saved);
  let input: VpnConnectionInput = serde_json::from_value(summary.clone()).unwrap();
  assert!(matches!(
    input.settings,
    VpnSettingsInput::Tailscale {
      accept_routes: true,
      ..
    }
  ));
  for field in [
    "url",
    "username",
    "password",
    "has_password",
    "auth_method",
    "target_ip",
  ] {
    let mut invalid = summary.clone();
    invalid[field] = json!(null);
    assert!(
      serde_json::from_value::<VpnConnectionInput>(invalid).is_err(),
      "accepted field: {field}"
    );
  }
}

#[test]
fn saved_connection_identity_cannot_switch_vpn_provider() {
  for starts_as_tailscale in [false, true] {
    let fixture = Fixture::new();
    let repository = fixture.repository();
    let mut original = save_request(None, Some("previous-test-secret"));
    let mut replacement = save_request(None, Some("replacement-test-secret"));
    let tailscale = VpnSettingsInput::Tailscale {
      hostname: None,
      accept_routes: false,
    };
    if starts_as_tailscale {
      original.connection.settings = tailscale;
    } else {
      replacement.connection.settings = tailscale;
    }
    let saved = repository.save(original).unwrap();
    let bytes = fixture.bytes();
    replacement.expected_revision = saved.revision;
    assert_eq!(
      repository.save(replacement).unwrap_err().code,
      "vpn_connection_provider_changed"
    );
    assert_eq!(fixture.bytes(), bytes);
    let retained = repository.connection("connection-one").unwrap();
    assert_eq!(
      retained.provider(),
      if starts_as_tailscale {
        VpnProvider::Tailscale
      } else {
        VpnProvider::Openconnect
      }
    );
  }
}

#[test]
fn host_connection_with_pending_browser_auth_returns_an_actionable_error() {
  let status = VpnStatus {
    provider: VpnProvider::Tailscale,
    state: VpnState::Starting,
    running: true,
    auth_url: Some("https://login.tailscale.com/a/testToken123".into()),
    ..VpnStatus::default()
  };
  let error = require_connected(&status).unwrap_err();
  assert_eq!(error.code, "vpn_sign_in_required");
  assert!(error.message.contains("VPN page"));
}

#[test]
fn input_accepts_legacy_openconnect_but_rejects_unknown_provider_fields() {
  let legacy = json!({
    "connection_id": "connection-one", "name": "Work VPN",
    "url": "https://vpn.example.test", "username": "test-user", "password": null,
  });
  for tagged in [false, true] {
    let mut value = legacy.clone();
    if tagged {
      value["provider"] = json!("openconnect");
    }
    let parsed: VpnConnectionInput = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(
      parsed.settings,
      VpnSettingsInput::Openconnect { password: None, .. }
    ));
    value["hostname"] = json!("unrelated-device-name");
    assert!(serde_json::from_value::<VpnConnectionInput>(value).is_err());
  }
  let mut unknown = legacy;
  unknown["provider"] = json!("unknown");
  assert!(serde_json::from_value::<VpnConnectionInput>(unknown).is_err());
  assert!(
    serde_json::from_value::<OpenVpnSignInRequest>(json!({
      "vpn_id": "tailscale-one", "url": "https://login.tailscale.com/a/testToken123",
    }))
    .is_err()
  );
}

#[test]
fn shared_foreign_container_and_incomplete_inventory_protect_saved_profiles() {
  let fixture = Fixture::new();
  let repository = fixture.repository();
  let saved = repository
    .save(save_request(None, Some("test-secret")))
    .unwrap();
  let bytes = fixture.bytes();
  let active = VpnSnapshot {
    connections: vec![VpnStatus {
      connection_id: Some("connection-one".into()),
      state: VpnState::Connected,
      shared_container: true,
      locally_connected: Some(false),
      ..VpnStatus::default()
    }],
    ..VpnSnapshot::default()
  };
  let incomplete = VpnSnapshot {
    discovery_warnings: vec!["Container inventory unavailable".into()],
    ..VpnSnapshot::default()
  };
  for (snapshot, code) in [
    (active, "vpn_connection_active"),
    (incomplete, "vpn_discovery_incomplete"),
  ] {
    assert_eq!(
      repository
        .save_with_status(save_request(saved.revision.clone(), None), &snapshot)
        .unwrap_err()
        .code,
      code
    );
    assert_eq!(
      repository
        .delete(
          &DeleteVpnConnectionRequest {
            expected_revision: saved.revision.clone(),
            connection_id: "connection-one".into(),
          },
          &snapshot
        )
        .unwrap_err()
        .code,
      code
    );
    assert_eq!(fixture.bytes(), bytes);
  }
}
