use super::*;
use ctl_ipc::credentials::{CredentialKind as StoredKind, legacy_scope_id, scope_id};
#[cfg(unix)]
use ctl_ipc::credentials::{Request, Response};

fn id() -> String {
  format!("{}:{}", "a".repeat(64), "b".repeat(64))
}

fn stored(scope: &str) -> StoredCredential {
  StoredCredential {
    credential_id: id(),
    scope_id: scope.into(),
    name: "Saved SSH credential bbbbbbbb".into(),
    kind: StoredKind::SshPassword,
    target: None,
    account: None,
    key_name: None,
    created_at_ms: Some(10),
    updated_at_ms: Some(20),
  }
}

fn inventory(credentials: Vec<StoredCredential>) -> Inventory {
  Inventory {
    credentials,
    complete: true,
    warning: None,
    metadata_import_required: false,
  }
}

fn vpn(settings: CredentialSettings) -> CredentialMetadata {
  CredentialMetadata {
    connection_id: "saved-vpn".into(),
    name: "Office VPN".into(),
    settings,
  }
}

#[test]
fn forget_accepts_only_exact_keychain_item_identifiers() {
  assert_eq!(backend_id(&format!("keychain:{}", id())).unwrap(), id());
  for invalid in [
    id(),
    format!("vpn:{}", id()),
    "keychain:all".into(),
    format!("keychain:{}", id().to_uppercase()),
    format!("keychain:{}\n", id()),
  ] {
    assert_eq!(
      backend_id(&invalid).unwrap_err().code,
      "invalid_credential_id"
    );
  }
}

#[test]
fn metadata_import_state_is_preserved_alongside_available_credentials() {
  let mut source = inventory(vec![stored(&"a".repeat(64))]);
  source.metadata_import_required = true;
  let result = snapshot(&BTreeMap::new(), Ok(source), Ok(vec![]), 123);
  assert!(result.metadata_import_required);
  assert_eq!(result.credentials.len(), 1);
  let result = snapshot(
    &BTreeMap::new(),
    Err(CommandErrorDto::new("credential_store_locked", "Locked")),
    Ok(vec![]),
    124,
  );
  assert!(!result.metadata_import_required);
  assert_eq!(result.sources[0].state, SourceState::Unavailable);
}

#[cfg(unix)]
#[tokio::test]
async fn old_helpers_are_reported_as_unsupported_for_noninteractive_inventory() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut command = tokio::process::Command::new("/bin/sh");
  command.args(["-c", "cat >/dev/null; printf '%s' '{\"type\":\"error\",\"code\":\"credential_request_invalid\",\"message\":\"private-fixture-canary\"}'", "legacy-helper"]);
  let error = helper::exchange(
    command,
    Request::ListMetadata,
    std::time::Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_unsupported");
  assert!(!error.message.contains("canary"));
}

#[test]
fn matching_uses_shared_scope_and_preserves_distinct_orphan_labels() {
  let target = crate::dto::ConnectionTargetDto::ssh("fixture");
  let scope = scope_id(&target.to_ssh_target().unwrap());
  let (hosts, complete) = host_metadata(&[
    NamedTarget {
      name: "Build".into(),
      target: target.clone(),
    },
    NamedTarget {
      name: "Alias".into(),
      target,
    },
  ]);
  assert!(complete);
  let result = snapshot(&hosts, Ok(inventory(vec![stored(&scope)])), Ok(vec![]), 123);
  assert_eq!(result.credentials[0].name, "Alias, Build");
  assert_eq!(result.credentials[0].target.as_deref(), Some("fixture"));
  assert_eq!(result.credentials[0].created_at_ms, Some(10));
  assert_eq!(result.credentials[0].updated_at_ms, Some(20));
  assert_eq!(result.checked_at_ms, 123);
  let result = snapshot(
    &BTreeMap::new(),
    Ok(inventory(vec![stored("removed-host")])),
    Ok(vec![]),
    0,
  );
  let serialized = serde_json::to_string(&result).unwrap();
  assert_eq!(result.credentials[0].name, "Saved SSH credential bbbbbbbb");
  assert!(!serialized.contains("scope_id"));
  assert!(!serialized.contains("key_name"));
}

#[test]
fn vpn_credential_associations_match_stable_and_exact_legacy_routes() {
  let vpn_target = |connection_id: &str| {
    serde_json::from_value::<crate::dto::ConnectionTargetDto>(serde_json::json!({
      "kind": "ssh", "destination": "target.internal",
      "vpn_connection_id": connection_id
    }))
    .unwrap()
  };
  let work = vpn_target("work");
  let personal = vpn_target("personal");
  let work_target = work.to_ssh_target().unwrap();
  let personal_target = personal.to_ssh_target().unwrap();
  let (hosts, complete) = host_metadata(&[
    NamedTarget {
      name: "Work".into(),
      target: work,
    },
    NamedTarget {
      name: "Personal".into(),
      target: personal,
    },
  ]);
  assert!(complete);
  assert_ne!(scope_id(&work_target), legacy_scope_id(&work_target));
  for (target, name) in [(&work_target, "Work"), (&personal_target, "Personal")] {
    for scope in [scope_id(target), legacy_scope_id(target)] {
      let row = keychain_record(stored(&scope), &hosts);
      assert_eq!(row.name, name);
      assert_eq!(row.target.as_deref(), Some("target.internal"));
    }
  }

  // Legacy labels are matched only for the actual route, never guessed sockets.
  let mut other_socket = work_target.clone();
  other_socket.gateways[0].vpn.as_mut().unwrap().socket_path =
    "/different-runtime/ctld.sock".into();
  assert_eq!(scope_id(&work_target), scope_id(&other_socket));
  let orphan = keychain_record(stored(&legacy_scope_id(&other_socket)), &hosts);
  assert_eq!(orphan.name, "Saved SSH credential bbbbbbbb");
  assert!(orphan.detail.unwrap().contains("No saved host"));
}

#[test]
fn invalid_host_hints_are_skipped_without_hiding_credentials() {
  assert!(
    !host_metadata(&[NamedTarget {
      name: "Local".into(),
      target: crate::dto::ConnectionTargetDto::Local
    }])
    .1
  );
  assert!(
    !host_metadata(&[NamedTarget {
      name: "\n".into(),
      target: crate::dto::ConnectionTargetDto::ssh("fixture")
    }])
    .1
  );
  assert!(
    !host_metadata(&[NamedTarget {
      name: "Host".into(),
      target: crate::dto::ConnectionTargetDto::ssh("-oProxyCommand=fixture")
    }])
    .1
  );
  assert!(
    !host_metadata(
      &(0..=MAX_TARGETS)
        .map(|_| NamedTarget {
          name: "Host".into(),
          target: crate::dto::ConnectionTargetDto::ssh("fixture"),
        })
        .collect::<Vec<_>>()
    )
    .1
  );
  let mut result = snapshot(
    &BTreeMap::new(),
    Ok(inventory(vec![stored("orphan")])),
    Ok(vec![]),
    0,
  );
  add_host_name_warning(&mut result);
  assert_eq!(result.credentials.len(), 1);
  assert_eq!(result.sources[0].state, SourceState::Partial);
  assert!(
    result.sources[0]
      .message
      .as_deref()
      .unwrap()
      .contains("credentials are still listed")
  );
}

#[test]
fn matching_key_passphrases_show_key_basename() {
  let target = crate::dto::ConnectionTargetDto::ssh("fixture");
  let scope = scope_id(&target.to_ssh_target().unwrap());
  let (hosts, _) = host_metadata(&[NamedTarget {
    name: "Build".into(),
    target,
  }]);
  let mut credential = stored(&scope);
  credential.kind = StoredKind::SshKeyPassphrase;
  credential.key_name = Some("/Users/example/.ssh/id_work".into());
  let row = keychain_record(credential, &hosts);
  assert_eq!(row.name, "Build · id_work");
  assert_eq!(row.detail.as_deref(), Some("SSH key: id_work"));
  let mut orphan = stored("orphan");
  orphan.name = "SSH key id_personal\n".into();
  assert_eq!(keychain_record(orphan, &hosts).name, "SSH key id_personal");
}

#[test]
fn source_failures_preserve_other_records_and_are_not_empty_success() {
  let result = snapshot(
    &BTreeMap::new(),
    Err(CommandErrorDto::new(
      "credential_store_locked",
      "Keychain is locked.",
    )),
    Ok(vec![vpn(CredentialSettings::Tailscale {
      hostname: Some("fixture-device".into()),
    })]),
    1,
  );
  assert_eq!(result.credentials.len(), 1);
  assert_eq!(result.sources[0].state, SourceState::Unavailable);
  assert_eq!(result.sources[1].state, SourceState::Ready);
  let result = snapshot(
    &BTreeMap::new(),
    Ok(inventory(vec![stored("orphan")])),
    Err(()),
    1,
  );
  assert_eq!(result.credentials.len(), 1);
  assert_eq!(result.sources[0].state, SourceState::Ready);
  assert_eq!(result.sources[1].state, SourceState::Unavailable);
}

#[test]
fn incomplete_invalid_or_duplicate_metadata_is_partial() {
  let mut invalid = stored("orphan");
  invalid.credential_id = "invalid".into();
  let mut data = inventory(vec![stored("orphan"), stored("orphan"), invalid]);
  data.warning = Some("untrusted helper warning".into());
  let result = snapshot(&BTreeMap::new(), Ok(data), Ok(vec![]), 1);
  assert_eq!(result.credentials.len(), 1);
  assert_eq!(result.sources[0].state, SourceState::Partial);
  assert!(
    !serde_json::to_string(&result)
      .unwrap()
      .contains("untrusted helper warning")
  );
  let mut data = inventory(vec![]);
  data.complete = false;
  let result = snapshot(&BTreeMap::new(), Ok(data), Ok(vec![]), 1);
  assert_eq!(result.sources[0].state, SourceState::Partial);
}

#[test]
fn vpn_rows_never_include_url_credentials_paths_queries_or_password_values() {
  let openconnect = vpn(CredentialSettings::Openconnect {
    url: "https://url-user:url-secret@gateway.invalid:8443/private/path?token=hidden#fragment"
      .into(),
    username: "vpn-user".into(),
    has_password: true,
  });
  let row = vpn_record(openconnect).unwrap();
  assert_eq!(row.target.as_deref(), Some("https://gateway.invalid:8443"));
  assert_eq!(row.account.as_deref(), Some("vpn-user"));
  assert_eq!(row.storage, CredentialStorage::VpnSettings);
  assert_eq!(row.action, CredentialAction::ManageVpn);
  assert_eq!(row.vpn_connection_id.as_deref(), Some("saved-vpn"));
  let serialized = serde_json::to_string(&row).unwrap();
  for secret in [
    "url-user",
    "url-secret",
    "private/path",
    "token=hidden",
    "fragment",
  ] {
    assert!(!serialized.contains(secret));
  }
  assert!(
    vpn_record(vpn(CredentialSettings::Openconnect {
      url: "https://gateway.invalid".into(),
      username: "vpn-user".into(),
      has_password: false,
    }))
    .is_none()
  );
  assert_eq!(
    url_origin("gateway.invalid:8443/private?token=hidden").as_deref(),
    Some("https://gateway.invalid:8443")
  );
  assert!(url_origin("https://[invalid").is_none());
}

#[test]
fn tailscale_metadata_does_not_claim_an_existing_login_or_offer_keychain_delete() {
  let row = vpn_record(vpn(CredentialSettings::Tailscale { hostname: None })).unwrap();
  assert_eq!(row.kind, CredentialKind::TailscaleSignIn);
  assert_eq!(row.storage, CredentialStorage::ContainerVolume);
  assert_eq!(row.action, CredentialAction::ManageVpn);
  assert!(row.detail.unwrap().contains("Sign-in state is unverified"));
  assert_eq!(row.created_at_ms, None);
  assert_eq!(row.updated_at_ms, None);
  assert!(backend_id(&row.credential_id).is_err());
}

#[cfg(unix)]
fn command(script: &str) -> tokio::process::Command {
  let mut command = tokio::process::Command::new("/bin/sh");
  command.args(["-c", script, "credential-fixture"]);
  command
}

#[cfg(unix)]
#[tokio::test]
async fn helper_receives_fixed_argument_and_eof_without_askpass_context() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let response = Response::Inventory {
    inventory: inventory(vec![stored("scope")]),
  };
  let mut command = command(
    r#"
    test "$1" = --credential-request || exit 1
    test -z "${CTLD_ASKPASS:-}" || exit 1
    test -z "${CTLD_ASKPASS_TOKEN:-}" || exit 1
    request=$(cat)
    test "$request" = '{"type":"list"}' || exit 1
    printf '%s' "$CREDENTIAL_FIXTURE_OUTPUT"
  "#,
  );
  command
    .env(
      "CREDENTIAL_FIXTURE_OUTPUT",
      serde_json::to_string(&response).unwrap(),
    )
    .env("CTLD_ASKPASS", "1")
    .env("CTLD_ASKPASS_TOKEN", "fixture-token");
  let actual = helper::exchange(command, Request::List, std::time::Duration::from_secs(2))
    .await
    .unwrap();
  assert_eq!(actual, response);
}

#[cfg(unix)]
#[tokio::test]
async fn helper_errors_and_stderr_are_sanitized() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut fixture = command(
    "cat >/dev/null; printf '%s' 'stderr-secret' >&2; printf '%s' \"$CREDENTIAL_FIXTURE_OUTPUT\"",
  );
  fixture.env(
    "CREDENTIAL_FIXTURE_OUTPUT",
    serde_json::to_string(&Response::Error {
      code: "credential_store_locked".into(),
      message: "helper-secret".into(),
    })
    .unwrap(),
  );
  let error = helper::exchange(fixture, Request::List, std::time::Duration::from_secs(2))
    .await
    .unwrap_err();
  assert_eq!(error.code, "credential_store_locked");
  assert!(!error.message.contains("secret"));
  let error = helper::exchange(
    command("exec 0<&-; printf 'stderr-secret' >&2; exit 2"),
    Request::List,
    std::time::Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_unsupported");
  assert!(!error.message.contains("secret"));
}

#[cfg(unix)]
#[test]
fn a_broken_input_pipe_does_not_mask_helper_failure_or_confirm_success() {
  let broken_pipe = || {
    Err(std::io::Error::new(
      std::io::ErrorKind::BrokenPipe,
      "input-secret",
    ))
  };
  let failure =
    helper::response_from_output(false, broken_pipe(), b"legacy-helper-secret").unwrap_err();
  assert_eq!(failure.code, "credential_helper_unsupported");
  assert!(!failure.message.contains("secret"));

  let output = serde_json::to_vec(&Response::Error {
    code: "credential_store_locked".into(),
    message: "helper-secret".into(),
  })
  .unwrap();
  let failure = helper::response_from_output(false, broken_pipe(), &output).unwrap_err();
  assert_eq!(failure.code, "credential_store_locked");
  assert!(!failure.message.contains("secret"));

  let output = serde_json::to_vec(&Response::Forgotten).unwrap();
  let failure = helper::response_from_output(true, broken_pipe(), &output).unwrap_err();
  assert_eq!(failure.code, "credential_helper_invalid_response");
  assert!(!failure.message.contains("secret"));
  assert_eq!(
    helper::response_from_output(true, Ok(()), &output).unwrap(),
    Response::Forgotten
  );
}

#[cfg(unix)]
#[tokio::test]
async fn helper_output_and_lifetime_are_bounded() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let error = helper::exchange(
    command(
      "cat >/dev/null; i=0; while [ \"$i\" -lt 1025 ]; do printf '%01024d' 0; i=$((i+1)); done; exec sleep 30",
    ),
    Request::List,
    std::time::Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_invalid_response");
  let error = helper::exchange(
    command("cat >/dev/null; exec sleep 30"),
    Request::List,
    std::time::Duration::from_millis(50),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_timeout");
}

#[cfg(unix)]
#[test]
fn known_helper_errors_preserve_actionable_categories_without_raw_messages() {
  for code in [
    "credential_store_unavailable",
    "credential_store_missing_entitlement",
    "credential_store_locked",
    "credential_forget_failed",
  ] {
    assert_eq!(helper::sanitized_error(code).code, code);
  }
  assert_eq!(
    helper::sanitized_error("credential_store_unsupported").code,
    "credentials_unsupported"
  );
}
#[test]
fn credential_associations_accept_the_same_remote_vpn_routes_as_the_broker() {
  let dto: crate::dto::ConnectionTargetDto = serde_json::from_value(serde_json::json!({
    "kind": "ssh", "destination": "target.internal",
    "gateways": [
      {"kind":"ssh", "gateway_id":"jump", "name":"Jump", "destination":"jump", "mode":"automatic"},
      {"kind":"vpn", "gateway_id":"vpn:work", "name":"Work", "destination":"work", "vpn_connection_id":"work", "mode":"automatic"}
    ]
  })).unwrap();
  let mut target = dto.to_ssh_target().unwrap();
  assert!(super::valid_target(&target));
  target.gateways[0].kind = ctl_ipc::GatewayKind::Socks5;
  target.gateways[0].port = Some(1080);
  assert!(!super::valid_target(&target));
}
