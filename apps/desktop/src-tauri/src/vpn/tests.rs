use std::fs;

use ctld_ipc::VpnState;
use serde_json::json;

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
    url: "https://vpn.example.test/group".into(),
    username: "test-user".into(),
    password: password.map(|password| Zeroizing::new(password.to_owned())),
    auth_method: None,
    target_ip: None,
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
  assert!(saved.connections[0].has_password);
  let revision = saved.revision.as_ref().unwrap();
  assert!(revision.starts_with("sha256:"));
  assert_eq!(revision.len(), 71);
  let response = serde_json::to_value(&saved).unwrap();
  assert!(response["connections"][0].get("password").is_none());
  assert!(!serde_json::to_string(&saved).unwrap().contains(password));
  let stored: serde_json::Value = serde_json::from_slice(&fixture.bytes()).unwrap();
  assert_eq!(stored["schema_version"], 1);
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
    &**repository.connection("connection-one").unwrap().password,
    "original-test-secret"
  );
  let changed = repository
    .save(save_request(
      edited.revision,
      Some("replacement-test-secret"),
    ))
    .unwrap();
  assert_eq!(
    &**repository.connection("connection-one").unwrap().password,
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
        &VpnStatus::default()
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
    let status = VpnStatus {
      connection_id: Some("connection-one".into()),
      state,
      ..VpnStatus::default()
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
      &VpnStatus::default(),
    )
    .unwrap();
  assert!(deleted.connections.is_empty());
  assert!(repository.connection("connection-one").is_err());
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
  invalid_url.connection.url = "http://vpn.example.test".into();
  assert!(repository.save(invalid_url).is_err());
  let mut invalid_target = save_request(saved.revision, None);
  invalid_target.connection.target_ip = Some("target.example.test".into());
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
  document["schema_version"] = json!(2);
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
    assert_eq!(&**connection.password, "adapter-test-secret");
    Ok(VpnStatus {
      connection_id: Some(connection.connection_id),
      state: VpnState::Connected,
      running: true,
      ..VpnStatus::default()
    })
  })
  .await
  .unwrap();
  assert_eq!(status.connection_id.as_deref(), Some("connection-one"));
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
