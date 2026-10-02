use super::*;
use ctl_ipc::credentials::{CredentialKind, Inventory, StoredCredential};

fn run_fixture(input: &[u8], handle: impl FnOnce(&Request) -> Response) -> Response {
  let mut output = Vec::new();
  run_with(input, &mut output, handle).unwrap();
  serde_json::from_slice(&output).unwrap()
}

#[test]
fn invalid_and_oversized_requests_never_reach_keychain() {
  for input in [
    b"not json".to_vec(),
    br#"{"type":"list","password":"fixture-secret"}"#.to_vec(),
    vec![b'x'; MAX_REQUEST_BYTES + 1],
  ] {
    let response = run_fixture(&input, |_| panic!("invalid request reached storage"));
    assert!(
      matches!(response, Response::Error { code, .. } if code == "credential_request_invalid")
    );
  }
}

#[test]
fn helper_dispatches_a_single_metadata_request_and_returns_json() {
  let response = run_fixture(br#"{"type":"list"}"#, |request| {
    assert_eq!(request, &Request::List);
    Response::Inventory {
      inventory: Inventory {
        credentials: Vec::new(),
        complete: true,
        warning: None,
        metadata_import_required: false,
      },
    }
  });
  assert!(matches!(response, Response::Inventory { inventory } if inventory.complete));
}

#[test]
fn importing_metadata_is_a_separate_explicit_operation() {
  let response = run_fixture(br#"{"type":"import_metadata"}"#, |request| {
    assert_eq!(request, &Request::ImportMetadata);
    Response::Imported
  });
  assert_eq!(response, Response::Imported);
  let response = run_fixture(br#"{"type":"import_metadata","authenticate":true}"#, |_| {
    panic!("unexpected fields reached storage")
  });
  assert!(matches!(response, Response::Error { code, .. } if code == "credential_request_invalid"));
}

#[test]
fn invalid_forget_identifier_is_rejected_before_platform_storage() {
  let response = handle(&Request::Forget {
    credential_id: "unrelated.service".into(),
  });
  assert!(matches!(response, Response::Error { code, .. } if code == "credential_request_invalid"));
}

#[test]
fn response_limit_preserves_valid_json_and_reports_truncation() {
  let credential = StoredCredential {
    credential_id: "a".repeat(129),
    scope_id: "b".repeat(64),
    name: "x".repeat(1024),
    kind: CredentialKind::SshCredential,
    target: Some("y".repeat(1024)),
    account: None,
    key_name: None,
    created_at_ms: None,
    updated_at_ms: None,
  };
  let output = encode_response(Response::Inventory {
    inventory: Inventory {
      credentials: vec![credential; 1024],
      complete: true,
      warning: None,
      metadata_import_required: false,
    },
  })
  .unwrap();
  assert!(output.len() < MAX_RESPONSE_BYTES);
  let Response::Inventory { inventory } = serde_json::from_slice(&output).unwrap() else {
    panic!("expected inventory");
  };
  assert_ne!(inventory.credentials, Vec::<StoredCredential>::new());
  assert!(inventory.credentials.len() < 1024);
  assert!(!inventory.complete);
  assert!(inventory.warning.is_some());
}

#[cfg(target_os = "macos")]
#[test]
fn keychain_errors_do_not_appear_as_a_successful_empty_inventory() {
  for (status, expected) in [
    (-34_018, "credential_store_missing_entitlement"),
    (-25_291, "credential_store_unavailable"),
    (-25_308, "credential_store_locked"),
    (-128, "credential_store_locked"),
    (-50, "credential_list_failed"),
  ] {
    let failure = crate::keychain::Error(security_framework::base::Error::from_code(status));
    let response = keychain_error(failure, "credential_list_failed");
    assert!(matches!(response, Response::Error { code, .. } if code == expected));
  }
}

#[cfg(target_os = "macos")]
#[test]
fn service_unavailable_does_not_tell_users_to_sign_the_daemon() {
  let failure = ctl_keychain_client::Error(-25_291).into();
  let Response::Error { message, .. } = keychain_error(failure, "credential_list_failed") else {
    panic!("unavailable service must return an error");
  };
  assert!(message.contains("login session"));
  assert!(!message.contains("signed"));
  assert!(!message.contains("entitlement"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_does_not_claim_to_have_an_empty_keychain() {
  assert!(
    matches!(handle(&Request::List), Response::Error { code, .. } if code == "credential_store_unsupported")
  );
}
