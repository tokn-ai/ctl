use super::*;

#[tokio::test]
async fn an_old_helper_is_rejected_before_receiving_a_new_operation() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (request, required) in [
    (
      credentials::Request::Discover {},
      ctl_ipc::HELPER_API_CONTRACT_V1_1_4,
    ),
    (
      credentials::Request::Clear {},
      ctl_ipc::HELPER_API_CONTRACT_V1_1_5,
    ),
  ] {
    let requested = Fixture::new();
    let old = ctl_ipc::HELPER_API_CONTRACT_V1_1_2;
    let mut fixture = command_with_contract(
      r#"printf 'request received' > "$CTL_HELPER_FIXTURE_REQUESTED"; cat >/dev/null; exit 99"#,
      old,
      &[ctl_ipc::HELPER_API_CONTRACT_V1_0_1, old],
    );
    fixture.env("CTL_HELPER_FIXTURE_REQUESTED", &requested.path);
    let error = exchange_credentials(fixture, request, Duration::from_secs(2))
      .await
      .unwrap_err();
    assert_eq!(error.code, "credential_helper_unsupported");
    assert!(
      error.message.contains(
        std::fs::canonicalize("/bin/sh")
          .unwrap()
          .to_string_lossy()
          .as_ref()
      )
    );
    assert!(error.message.contains("1.1.2 (build 2)"));
    assert!(
      error
        .message
        .contains(&format!("{required} (build {})", required.build))
    );
    assert!(!requested.path.exists());
  }
}

#[tokio::test]
async fn newest_helpers_accept_the_explicitly_retained_initial_contract() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for request in [
    credentials::Request::List,
    credentials::Request::ListMetadata,
    credentials::Request::ImportMetadata,
  ] {
    assert_eq!(
      credential_contract(&request),
      ctl_ipc::HELPER_API_CONTRACT_V1_0_1
    );
    let expected = match &request {
      credentials::Request::List | credentials::Request::ListMetadata => {
        credentials::Response::Inventory {
          inventory: credentials::Inventory {
            credentials: Vec::new(),
            complete: true,
            warning: None,
            metadata_import_required: false,
          },
        }
      }
      credentials::Request::ImportMetadata => credentials::Response::Imported,
      _ => unreachable!(),
    };
    let mut fixture = command(
      r#"request=$(cat); test "$request" = "$CTL_HELPER_FIXTURE_REQUEST" || exit 99; printf '%s' "$CTL_HELPER_FIXTURE_REPLY""#,
    );
    fixture.env(
      "CTL_HELPER_FIXTURE_REQUEST",
      serde_json::to_string(&request).unwrap(),
    );
    fixture.env(
      "CTL_HELPER_FIXTURE_REPLY",
      serde_json::to_string(&expected).unwrap(),
    );
    assert_eq!(
      exchange_credentials(fixture, request, Duration::from_secs(2))
        .await
        .unwrap(),
      expected,
    );
  }
  let request = identities::Request::ListMetadata { paths: Vec::new() };
  assert_eq!(
    identity_contract(&request),
    ctl_ipc::HELPER_API_CONTRACT_V1_0_1
  );
  let initial = ctl_ipc::HELPER_API_CONTRACT_V1_0_1;
  let expected = identities::Response::Inventory {
    inventory: identities::Inventory {
      identity_files: Vec::new(),
      complete: true,
      file_discovery_complete: true,
      warning: None,
      keychain_available: true,
      keychain_error: None,
      metadata_import_required: false,
    },
  };
  let mut fixture = command_with_contract(
    r#"request=$(cat); test "$request" = '{"type":"list_metadata","paths":[]}' || exit 99; printf '%s' "$CTL_HELPER_FIXTURE_REPLY""#,
    initial,
    &[initial],
  );
  fixture.env(
    "CTL_HELPER_FIXTURE_REPLY",
    serde_json::to_string(&expected).unwrap(),
  );
  assert_eq!(
    exchange_identity(fixture, request, Duration::from_secs(2))
      .await
      .unwrap(),
    expected,
  );
}

#[tokio::test]
async fn secret_mutations_reject_every_pre_revocation_helper_before_sending_input() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let historical = &[
    ctl_ipc::HELPER_API_CONTRACT_V1_0_1,
    ctl_ipc::HELPER_API_CONTRACT_V1_1_2,
    ctl_ipc::HELPER_API_CONTRACT_V1_1_3,
    ctl_ipc::HELPER_API_CONTRACT_V1_1_4,
  ];
  for (index, old) in historical.iter().copied().enumerate() {
    for request in [
      credentials::Request::Clear {},
      credentials::Request::Forget {
        credential_id: format!("{}:{}", "a".repeat(64), "b".repeat(64)),
      },
    ] {
      assert_eq!(
        credential_contract(&request),
        ctl_ipc::HELPER_API_CONTRACT_V1_1_5
      );
      let requested = Fixture::new();
      let mut fixture = command_with_contract(
        r#"printf received > "$CTL_HELPER_FIXTURE_REQUESTED"; cat >/dev/null; exit 99"#,
        old,
        &historical[..=index],
      );
      fixture.env("CTL_HELPER_FIXTURE_REQUESTED", &requested.path);
      let error = exchange_credentials(fixture, request, Duration::from_secs(2))
        .await
        .unwrap_err();
      assert_eq!(error.code, "credential_helper_unsupported");
      assert!(error.message.contains("requires 1.1.5 (build 5)"));
      assert!(!requested.path.exists());
    }
    for request in [
      identities::Request::Save {
        path: "/tmp/private-key".into(),
        file_version: "a".repeat(64),
        passphrase: Zeroizing::new("private-fixture-canary".into()),
      },
      identities::Request::Forget {
        identity_id: "a".repeat(64),
      },
    ] {
      assert_eq!(
        identity_contract(&request),
        ctl_ipc::HELPER_API_CONTRACT_V1_1_5
      );
      let requested = Fixture::new();
      let mut fixture = command_with_contract(
        r#"printf received > "$CTL_HELPER_FIXTURE_REQUESTED"; cat >/dev/null; exit 99"#,
        old,
        &historical[..=index],
      );
      fixture.env("CTL_HELPER_FIXTURE_REQUESTED", &requested.path);
      let error = exchange_identity(fixture, request, Duration::from_secs(2))
        .await
        .unwrap_err();
      assert_eq!(error.code, "credential_helper_unsupported");
      assert!(error.message.contains("requires 1.1.5 (build 5)"));
      assert!(!error.to_string().contains("canary"));
      assert!(!requested.path.exists());
    }
  }
}

#[tokio::test]
async fn revocation_aware_helpers_keep_the_existing_mutation_wire_shapes() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let request = credentials::Request::Forget {
    credential_id: format!("{}:{}", "a".repeat(64), "b".repeat(64)),
  };
  let mut fixture = command(
    r#"request=$(cat); test "$request" = "$CTL_HELPER_FIXTURE_REQUEST" || exit 99; printf '%s' '{"type":"forgotten"}'"#,
  );
  fixture.env(
    "CTL_HELPER_FIXTURE_REQUEST",
    serde_json::to_string(&request).unwrap(),
  );
  assert_eq!(
    exchange_credentials(fixture, request, Duration::from_secs(2))
      .await
      .unwrap(),
    credentials::Response::Forgotten,
  );
  for (request, expected) in [
    (
      identities::Request::Save {
        path: "/tmp/private-key".into(),
        file_version: "a".repeat(64),
        passphrase: Zeroizing::new("private-fixture-canary".into()),
      },
      identities::Response::Saved,
    ),
    (
      identities::Request::Forget {
        identity_id: "a".repeat(64),
      },
      identities::Response::Forgotten,
    ),
  ] {
    let mut fixture = command(
      r#"request=$(cat); test "$request" = "$CTL_HELPER_FIXTURE_REQUEST" || exit 99; printf '%s' "$CTL_HELPER_FIXTURE_REPLY""#,
    );
    fixture.env(
      "CTL_HELPER_FIXTURE_REQUEST",
      serde_json::to_string(&request).unwrap(),
    );
    fixture.env(
      "CTL_HELPER_FIXTURE_REPLY",
      serde_json::to_string(&expected).unwrap(),
    );
    assert_eq!(
      exchange_identity(fixture, request, Duration::from_secs(2))
        .await
        .unwrap(),
      expected
    );
  }
}

#[tokio::test]
async fn a_newer_advertised_version_does_not_imply_unlisted_older_support() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let requested = Fixture::new();
  let latest = ctl_ipc::HELPER_API_VERSION;
  let mut fixture = command_with_contract(
    r#"printf 'request received' > "$CTL_HELPER_FIXTURE_REQUESTED"; cat >/dev/null"#,
    latest,
    &[latest],
  );
  fixture.env("CTL_HELPER_FIXTURE_REQUESTED", &requested.path);
  let error = exchange_credentials(
    fixture,
    credentials::Request::ListMetadata,
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_unsupported");
  assert!(error.message.contains("requires 1.0.1"));
  assert!(!requested.path.exists());
}

#[tokio::test]
async fn malformed_or_failed_metadata_does_not_send_identity_secret_input() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (status, expected_code) in [
    (0, "credential_helper_metadata_invalid"),
    (2, "credential_helper_failed"),
  ] {
    let requested = Fixture::new();
    let script = format!(
      r#"if [ "$1" = --component-info ]; then printf '%s' 'parser-private-fixture-canary'; printf '%s' 'stderr-private-fixture-canary' >&2; exit {status}; fi; printf 'request received' > "$CTL_HELPER_FIXTURE_REQUESTED"; cat >/dev/null; exit 99"#,
    );
    let mut fixture = Command::new("/bin/sh");
    fixture.args(["-c", &script, "credential-fixture"]);
    fixture.env("CTL_HELPER_FIXTURE_REQUESTED", &requested.path);
    let error = exchange_identity(
      fixture,
      identities::Request::Save {
        path: "/tmp/private-key".into(),
        file_version: "version".into(),
        passphrase: Zeroizing::new("secret-private-fixture-canary".into()),
      },
      Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, expected_code);
    assert!(
      error
        .message
        .contains("available ctld_helper contract is unknown")
    );
    assert!(error.message.contains("requires 1.1.5 (build 5)"));
    assert!(!format!("{error:?}").contains("canary"));
    assert!(!requested.path.exists());
  }
}

#[tokio::test]
async fn failed_operation_output_is_distinct_from_a_structured_rejection() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for (script, expected_code) in [
    (
      r"cat >/dev/null; printf '%s' 'parser-private-fixture-canary'; printf '%s' 'stderr-private-fixture-canary' >&2; exit 2",
      "credential_helper_failed",
    ),
    (
      r#"cat >/dev/null; printf '%s' '{"type":"error","code":"credential_request_invalid","message":"private-fixture-canary"}'; exit 2"#,
      "credential_helper_unsupported",
    ),
    (
      r#"cat >/dev/null; printf '%s' '{"type":"discovered","inventory":{"entries":[],"complete":true,"warnings":[]}}'; exit 2"#,
      "credential_helper_failed",
    ),
  ] {
    let error = exchange_credentials(
      command(script),
      credentials::Request::Discover {},
      Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, expected_code);
    assert!(
      error.message.contains(
        std::fs::canonicalize("/bin/sh")
          .unwrap()
          .to_string_lossy()
          .as_ref()
      )
    );
    assert!(error.message.contains(&format!(
      "{} (build {})",
      ctl_ipc::HELPER_API_VERSION,
      ctl_ipc::HELPER_API_BUILD
    )));
    assert!(!error.to_string().contains("canary"));
  }
}

#[tokio::test]
async fn invalid_identity_metadata_contents_are_not_reported_as_unsupported() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let error = exchange_identity(
    command(r#"cat >/dev/null; printf '%s' '{"type":"error","code":"identity_invalid_request","message":"private-fixture-canary"}'"#),
    identities::Request::ListMetadata { paths: vec!["/tmp/key".into()] },
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "identity_invalid_request");
  assert!(!error.to_string().contains("canary"));
}

#[tokio::test]
async fn selected_executable_paths_are_escaped_in_diagnostics() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let error = compatibility::check(
    Command::new("/missing/helper\n\u{1b}[2J"),
    ctl_ipc::HELPER_API_CONTRACT_V1_1_4,
  )
  .await
  .err()
  .unwrap();
  assert!(!error.message.contains('\n'));
  assert!(!error.message.contains('\u{1b}'));
  assert!(error.message.contains("\\n"));
  assert!(error.message.contains("requires 1.1.4"));
}

#[tokio::test]
async fn swapping_the_selected_symlink_cannot_redirect_the_private_request() {
  use std::os::unix::fs::{PermissionsExt as _, symlink};
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;

  let directory = FixtureDirectory::new();
  let checked = directory.path.join("checked-helper");
  let replacement = directory.path.join("replacement-helper");
  let selected = directory.path.join("ctld");
  let checked_request = directory.path.join("checked-request");
  let replacement_request = directory.path.join("replacement-request");
  std::fs::write(
    &checked,
    r#"#!/bin/sh
    if [ "$1" = --component-info ]; then
      /bin/rm "$CTL_HELPER_FIXTURE_SELECTED"
      /bin/ln -s "$CTL_HELPER_FIXTURE_REPLACEMENT" "$CTL_HELPER_FIXTURE_SELECTED"
      printf '%s' "$CTL_HELPER_FIXTURE_METADATA"
      exit 0
    fi
    test "$1" = --credential-request || exit 99
    request=$(/bin/cat)
    test "$request" = '{"type":"discover"}' || exit 99
    printf received > "$CTL_HELPER_FIXTURE_CHECKED_REQUEST"
    printf '%s' '{"type":"discovered","inventory":{"entries":[],"complete":true,"warnings":[]}}'
    "#,
  )
  .unwrap();
  std::fs::write(
    &replacement,
    r#"#!/bin/sh
    printf received > "$CTL_HELPER_FIXTURE_REPLACEMENT_REQUEST"
    exit 99
    "#,
  )
  .unwrap();
  for helper in [&checked, &replacement] {
    std::fs::set_permissions(helper, std::fs::Permissions::from_mode(0o700)).unwrap();
  }
  symlink(&checked, &selected).unwrap();
  let mut fixture = Command::new(&selected);
  fixture
    .env("CTL_HELPER_FIXTURE_SELECTED", &selected)
    .env("CTL_HELPER_FIXTURE_REPLACEMENT", &replacement)
    .env("CTL_HELPER_FIXTURE_CHECKED_REQUEST", &checked_request)
    .env(
      "CTL_HELPER_FIXTURE_REPLACEMENT_REQUEST",
      &replacement_request,
    )
    .env(
      "CTL_HELPER_FIXTURE_METADATA",
      metadata(
        ctl_ipc::HELPER_API_VERSION,
        ctl_ipc::SUPPORTED_HELPER_API_VERSIONS,
      ),
    );
  assert!(matches!(
    exchange_credentials(
      fixture,
      credentials::Request::Discover {},
      Duration::from_secs(2)
    )
    .await
    .unwrap(),
    credentials::Response::Discovered { .. },
  ));
  assert_eq!(
    selected.canonicalize().unwrap(),
    replacement.canonicalize().unwrap()
  );
  assert!(checked_request.exists());
  assert!(!replacement_request.exists());
}

#[tokio::test]
async fn helper_resolution_respects_command_path_and_working_directory() {
  use std::os::unix::fs::PermissionsExt as _;
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;

  let directory = FixtureDirectory::new();
  let bin = directory.path.join("bin");
  std::fs::create_dir(&bin).unwrap();
  let executable = bin.join("ctld");
  std::fs::write(
    &executable,
    r#"#!/bin/sh
    if [ "$1" = --component-info ]; then
      printf '%s' "$CTL_HELPER_FIXTURE_METADATA"
      exit 0
    fi
    request=$(/bin/cat)
    test "$request" = '{"type":"clear"}' || exit 99
    printf '%s' '{"type":"cleared","credential_count":0,"identity_count":0}'
    "#,
  )
  .unwrap();
  std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
  let mut fixture = Command::new("ctld");
  fixture.current_dir(&directory.path).env("PATH", "bin").env(
    "CTL_HELPER_FIXTURE_METADATA",
    metadata(
      ctl_ipc::HELPER_API_VERSION,
      ctl_ipc::SUPPORTED_HELPER_API_VERSIONS,
    ),
  );
  assert_eq!(
    exchange_credentials(
      fixture,
      credentials::Request::Clear {},
      Duration::from_secs(2)
    )
    .await
    .unwrap(),
    credentials::Response::Cleared {
      credential_count: 0,
      identity_count: 0
    },
  );
}

struct FixtureDirectory {
  path: std::path::PathBuf,
}

impl FixtureDirectory {
  fn new() -> Self {
    let path = std::env::temp_dir().join(format!("ctl-helper-preflight-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    Self { path }
  }
}

impl Drop for FixtureDirectory {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.path);
  }
}
