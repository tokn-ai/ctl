use super::*;
use ctl_ipc::identities::{FileState, PassphraseState};

fn file() -> IdentityFile {
  IdentityFile {
    identity_id: "a".repeat(64),
    path: "/fixture/keys/work".into(),
    display_path: "~/keys/work".into(),
    file_version: Some("b".repeat(64)),
    key_type: Some("ssh-ed25519".into()),
    fingerprint: Some("SHA256:fixture".into()),
    encrypted: Some(true),
    file_state: FileState::Ready,
    passphrase_state: PassphraseState::Saved,
    detail: None,
  }
}

#[test]
fn host_names_are_hints_and_paths_are_normalized_without_opening_key_contents() {
  let mut target = ConnectionTargetDto::ssh("fixture");
  if let ConnectionTargetDto::Ssh { identity_file, .. } = &mut target {
    *identity_file = Some("~/keys/./work".into());
  }
  let targets = vec![
    NamedTarget {
      name: "Build".into(),
      target: target.clone(),
    },
    NamedTarget {
      name: "Test".into(),
      target,
    },
  ];
  let hints = host_paths(&targets, Some(Path::new("/fixture")));
  assert!(hints.complete);
  assert_eq!(
    hints.names["/fixture/keys/work"],
    BTreeSet::from(["Build".into(), "Test".into()])
  );
  assert!(normalize_path("~/keys/work\nsecret", Some(Path::new("/fixture"))).is_none());
  assert!(normalize_path("~someone/key", Some(Path::new("/fixture"))).is_none());
}

#[test]
fn duplicate_or_invalid_metadata_cannot_create_ambiguous_actions() {
  let mut invalid = file();
  invalid.identity_id = "all".into();
  let inventory = Inventory {
    identity_files: vec![file(), file(), invalid],
    complete: true,
    warning: Some("raw-helper-message".into()),
    file_discovery_complete: true,
    keychain_available: true,
    keychain_error: None,
    metadata_import_required: false,
  };
  let names = BTreeMap::from([(
    "/fixture/keys/work".into(),
    BTreeSet::from(["Build".into()]),
  )]);
  let snapshot = snapshot(inventory, &names, true);
  assert_eq!(snapshot.identity_files.len(), 1);
  assert!(!snapshot.complete);
  let json = serde_json::to_string(&snapshot).unwrap();
  assert!(json.contains("Build"));
  assert!(!json.contains("raw-helper-message"));
  assert!(!json.contains("\"passphrase\":"));
  assert!(!json.contains("private_key"));
}

#[test]
fn keychain_failures_keep_their_category_without_forwarding_helper_text() {
  for (code, expected) in [
    ("identity_keychain_locked", "locked, denied, or cancelled"),
    ("identity_keychain_unavailable", "properly signed"),
    ("private-fixture-canary", "could not be inspected"),
  ] {
    let inventory = Inventory {
      identity_files: vec![file()],
      complete: false,
      warning: Some("private-fixture-canary".into()),
      file_discovery_complete: true,
      keychain_available: false,
      keychain_error: Some(code.into()),
      metadata_import_required: true,
    };
    let result = snapshot(inventory, &BTreeMap::new(), true);
    assert!(!result.keychain_available);
    assert!(result.metadata_import_required);
    assert!(
      result
        .keychain_message
        .as_deref()
        .unwrap()
        .contains(expected)
    );
    assert!(!serde_json::to_string(&result).unwrap().contains("canary"));
  }
}

#[test]
fn pending_import_does_not_hide_an_independent_file_discovery_failure() {
  for files_complete in [true, false] {
    let inventory = Inventory {
      identity_files: vec![file()],
      complete: false,
      file_discovery_complete: files_complete,
      warning: None,
      keychain_available: true,
      keychain_error: None,
      metadata_import_required: true,
    };
    let result = snapshot(inventory, &BTreeMap::new(), true);
    assert!(!result.complete);
    assert!(result.metadata_import_required);
    assert_eq!(result.warning.is_some(), !files_complete);
  }
}

#[cfg(unix)]
#[tokio::test]
async fn old_identity_helpers_cannot_silently_run_interactive_inventory() {
  let mut command = tokio::process::Command::new("/bin/sh");
  command.args(["-c", "cat >/dev/null; printf '%s' '{\"type\":\"error\",\"code\":\"identity_invalid_request\",\"message\":\"private-fixture-canary\"}'", "legacy-identity-helper"]);
  let error = exchange_with(command, Request::ListMetadata { paths: Vec::new() })
    .await
    .err()
    .unwrap();
  assert_eq!(error.code, "credential_helper_unsupported");
  assert!(!error.message.contains("canary"));
}

#[tokio::test]
async fn invalid_mutations_are_rejected_before_spawning_a_helper() {
  let request = SaveRequest {
    path: "/fixture/key".into(),
    file_version: "invalid".into(),
    passphrase: Zeroizing::new("private-fixture-canary".into()),
  };
  let error = save_identity_passphrase(request).await.unwrap_err();
  assert_eq!(error.code, "identity_invalid_request");
  assert!(!error.message.contains("canary"));
  let error = forget_identity_passphrase(ForgetRequest {
    identity_id: "all".into(),
  })
  .await
  .unwrap_err();
  assert_eq!(error.code, "identity_invalid_request");
}

#[test]
fn request_budget_leaves_room_for_json_escaping() {
  let names = (0..512)
    .map(|index| {
      (
        format!("/fixture/{index}/{}", "\\\"".repeat(2000)),
        BTreeSet::new(),
      )
    })
    .collect();
  let paths = bounded_paths(&names);
  assert_ne!(paths, Vec::<String>::new());
  assert!(paths.len() < 512);
  let encoded = serde_json::to_vec(&Request::List { paths }).unwrap();
  assert!(encoded.len() <= ctl_ipc::identities::MAX_REQUEST_BYTES);
}

#[cfg(unix)]
#[tokio::test]
async fn secret_bearing_requests_use_stdin_and_errors_remain_sanitized() {
  let mut command = tokio::process::Command::new("/bin/sh");
  command.args(["-c", concat!(
    "test \"$1\" = --identity-request || exit 2; ",
    "test \"$#\" = 1 || exit 2; ",
    "test -z \"${CTLD_ASKPASS_TOKEN:-}\" || exit 2; ",
    "test -z \"${CTLD_IDENTITY_ASKPASS:-}\" || exit 2; ",
    "test -z \"${CTLD_IDENTITY_ASKPASS_TOKEN:-}\" || exit 2; ",
    "cat >/dev/null; ",
    "printf '%s' '{\"type\":\"error\",\"code\":\"identity_unlock_failed\",\"message\":\"private-fixture-canary\"}'"
  ), "identity-fixture"]);
  command
    .env("CTLD_ASKPASS_TOKEN", "unrelated-context")
    .env("CTLD_IDENTITY_ASKPASS", "1")
    .env("CTLD_IDENTITY_ASKPASS_TOKEN", "unrelated-context");
  let error = exchange_with(
    command,
    Request::Save {
      path: "/fixture/key".into(),
      file_version: "b".repeat(64),
      passphrase: Zeroizing::new("private-fixture-canary".into()),
    },
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "identity_unlock_failed");
  assert!(!error.message.contains("canary"));
}

#[cfg(unix)]
#[test]
fn missing_identity_leaf_keeps_canonical_parent_and_host_association() {
  use std::os::unix::fs::symlink;

  let directory =
    std::env::temp_dir().join(format!("identity-path-fixture-{}", uuid::Uuid::new_v4()));
  let actual = directory.join("actual");
  let alias = directory.join("alias");
  std::fs::create_dir_all(&actual).unwrap();
  symlink(&actual, &alias).unwrap();
  let expected = std::fs::canonicalize(&actual).unwrap().join("missing-key");
  let normalized = normalize_path(alias.join("missing-key").to_str().unwrap(), None).unwrap();
  assert_eq!(normalized, expected.to_str().unwrap());

  let mut missing = file();
  missing.path.clone_from(&normalized);
  missing.file_state = FileState::Missing;
  missing.file_version = None;
  let inventory = Inventory {
    identity_files: vec![missing],
    complete: true,
    warning: None,
    file_discovery_complete: true,
    keychain_available: true,
    keychain_error: None,
    metadata_import_required: false,
  };
  let names = BTreeMap::from([(normalized, BTreeSet::from(["Build".into()]))]);
  let snapshot = snapshot(inventory, &names, true);
  assert_eq!(snapshot.identity_files[0].used_by, ["Build"]);
  std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_host_labels_do_not_hide_identity_paths_and_gateway_truncation_is_reported() {
  let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
    "kind": "ssh", "destination": "fixture", "identity_file": "/fixture/key",
    "gateways": (0..9).map(|index| serde_json::json!({
      "gateway_id": format!("gateway-{index}"), "name": "Gateway", "destination": "gateway.example.test",
      "identity_file": format!("/fixture/gateway-{index}"), "mode": "automatic",
    })).collect::<Vec<_>>(),
  })).unwrap();
  let hints = host_paths(
    &[NamedTarget {
      name: "Invalid\nlabel".into(),
      target,
    }],
    None,
  );
  assert!(!hints.complete);
  assert!(hints.names.contains_key("/fixture/key"));
  assert_eq!(hints.names.len(), 9);
  assert!(hints.names.values().all(BTreeSet::is_empty));
}

#[test]
fn identity_file_none_is_an_intentional_absence_not_a_discovery_error() {
  let mut target = ConnectionTargetDto::ssh("fixture");
  if let ConnectionTargetDto::Ssh { identity_file, .. } = &mut target {
    *identity_file = Some("none".into());
  }
  let hints = host_paths(
    &[NamedTarget {
      name: "Build".into(),
      target,
    }],
    None,
  );
  assert!(hints.complete);
  assert_eq!(hints.names, BTreeMap::<String, BTreeSet<String>>::new());
}

#[test]
fn configured_identity_tokens_expand_only_known_context_and_percent_is_not_expanded_twice() {
  let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
    "kind": "ssh", "destination": "builder@alias", "hostname": "build.example.test",
    "user": "builder", "port": 2222, "identity_file": "%d/keys/%%-%h-%r-%p-%n",
  }))
  .unwrap();
  let hints = host_paths(
    &[NamedTarget {
      name: "Build".into(),
      target,
    }],
    Some(Path::new("/fixture")),
  );
  assert!(hints.complete);
  assert!(
    hints
      .names
      .contains_key("/fixture/keys/%-build.example.test-builder-2222-build.example.test")
  );

  for value in [
    "~/keys/%h",
    "~/keys/%r",
    "~/keys/%p",
    "~/keys/%u",
    "~/keys/${KEY_NAME}",
  ] {
    let mut target = ConnectionTargetDto::ssh("alias");
    if let ConnectionTargetDto::Ssh { identity_file, .. } = &mut target {
      *identity_file = Some(value.into());
    }
    let hints = host_paths(
      &[NamedTarget {
        name: "Build".into(),
        target,
      }],
      Some(Path::new("/fixture")),
    );
    assert!(!hints.complete, "unresolved value: {value}");
    assert!(hints.names.is_empty(), "unresolved value: {value}");
  }
}

#[test]
fn original_host_token_matches_the_actual_ssh_argument_for_aliases_and_overrides() {
  for (alias, hostname, expected) in [
    (
      Some("config-alias"),
      Some("override.example.test"),
      "config-alias",
    ),
    (None, Some("override.example.test"), "override.example.test"),
    (None, None, "destination"),
  ] {
    let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
      "kind": "ssh", "destination": "builder@destination", "ssh_config_alias": alias,
      "hostname": hostname, "identity_file": "/fixture/keys/%n",
      "gateways": [{ "gateway_id": "gateway", "name": "Gateway", "destination": "old-alias",
        "hostname": "jump.example.test", "identity_file": "/fixture/keys/%n", "mode": "automatic" }],
    })).unwrap();
    let hints = host_paths(
      &[NamedTarget {
        name: "Build".into(),
        target,
      }],
      None,
    );
    assert!(hints.complete);
    assert!(
      hints
        .names
        .contains_key(&format!("/fixture/keys/{expected}"))
    );
    assert!(hints.names.contains_key("/fixture/keys/jump.example.test"));
  }
}
