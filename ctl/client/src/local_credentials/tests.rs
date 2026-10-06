use super::*;

fn command(script: &str) -> Command {
  let mut command = Command::new("/bin/sh");
  command.args(["-c", script, "credential-fixture"]);
  command
}

#[tokio::test]
async fn helper_receives_fixed_argument_and_eof_without_askpass_context() {
  let mut command = command(
    r#"
    test "$1" = --credential-request || exit 1
    test -z "${CTLD_ASKPASS:-}${CTLD_ASKPASS_TOKEN:-}" || exit 1
    test -z "${CTLD_IDENTITY_ASKPASS:-}${CTLD_IDENTITY_ASKPASS_SOCKET:-}${CTLD_IDENTITY_ASKPASS_TOKEN:-}" || exit 1
    request=$(cat)
    test "$request" = '{"type":"list_metadata"}' || exit 1
    printf '%s' '{"type":"inventory","inventory":{"credentials":[],"complete":true,"warning":null,"metadata_import_required":false}}'
    "#,
  );
  for variable in [
    "CTLD_ASKPASS",
    "CTLD_ASKPASS_TOKEN",
    "CTLD_IDENTITY_ASKPASS",
    "CTLD_IDENTITY_ASKPASS_SOCKET",
    "CTLD_IDENTITY_ASKPASS_TOKEN",
  ] {
    command.env(variable, "private-fixture-canary");
  }
  let response = exchange_credentials(
    command,
    credentials::Request::ListMetadata,
    Duration::from_secs(2),
  )
  .await
  .unwrap();
  assert!(
    matches!(response, credentials::Response::Inventory { inventory } if inventory.credentials.is_empty() && inventory.complete)
  );
}

#[tokio::test]
async fn clear_reports_confirmed_counts_and_categorizes_old_or_failed_helpers() {
  let fixture = command(
    r#"request=$(cat); test "$request" = '{"type":"clear"}' || exit 1; printf '%s' '{"type":"cleared","credential_count":2,"identity_count":3}'"#,
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
      credential_count: 2,
      identity_count: 3,
    }
  );
  for (helper_code, expected_code) in [
    (
      "credential_request_invalid",
      "credential_helper_unsupported",
    ),
    ("credential_clear_failed", "credential_clear_failed"),
  ] {
    let fixture = command(&format!(
      r#"cat >/dev/null; printf '%s' '{{"type":"error","code":"{helper_code}","message":"private-fixture-canary"}}'; exit 2"#,
    ));
    let error = exchange_credentials(
      fixture,
      credentials::Request::Clear {},
      Duration::from_secs(2),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, expected_code);
    assert!(!error.to_string().contains("canary"));
    if helper_code == "credential_clear_failed" {
      assert!(error.message.contains("may already have been removed"));
    }
  }
}

#[tokio::test]
async fn old_helpers_never_receive_interactive_inventory_fallback() {
  let fixture = command(
    r#"request=$(cat); test "$request" = '{"type":"list_metadata"}' || exit 1; printf '%s' '{"type":"error","code":"credential_request_invalid","message":"private-fixture-canary"}'"#,
  );
  let error = exchange_credentials(
    fixture,
    credentials::Request::ListMetadata,
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_unsupported");
  assert!(!error.to_string().contains("canary"));

  let fixture = command(
    r#"request=$(cat); test "$request" = '{"type":"list_metadata","paths":[]}' || exit 1; printf '%s' '{"type":"error","code":"identity_invalid_request","message":"private-fixture-canary"}'"#,
  );
  let error = exchange_identity(
    fixture,
    identities::Request::ListMetadata { paths: vec![] },
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_unsupported");
}

#[tokio::test]
async fn helper_errors_and_stderr_are_sanitized() {
  let fixture = command(
    r#"cat >/dev/null; printf '%s' 'stderr-private-fixture-canary' >&2; printf '%s' '{"type":"error","code":"credential_store_locked","message":"helper-private-fixture-canary"}'; exit 2"#,
  );
  let error = exchange_credentials(
    fixture,
    credentials::Request::ListMetadata,
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_store_locked");
  assert!(!format!("{error:?}").contains("canary"));
  let error = exchange_credentials(
    command("cat >/dev/null; printf '%s' 'parser-private-fixture-canary'"),
    credentials::Request::ListMetadata,
    Duration::from_secs(2),
  )
  .await
  .unwrap_err();
  assert_eq!(error.code, "credential_helper_invalid_response");
  assert!(!error.to_string().contains("canary"));
}

#[test]
fn a_broken_input_pipe_does_not_confirm_success() {
  let output = Output {
    success: true,
    input_result: Err(std::io::Error::new(
      std::io::ErrorKind::BrokenPipe,
      "private-fixture-canary",
    )),
    bytes: b"{\"type\":\"forgotten\"}".to_vec(),
  };
  assert_eq!(
    validate_completion(&output).unwrap_err().code,
    "credential_helper_invalid_response"
  );
}

#[tokio::test]
async fn input_and_output_progress_concurrently() {
  let output = exchange(
    command("dd if=/dev/zero bs=1024 count=128 2>/dev/null; cat >/dev/null"),
    "--credential-request",
    Zeroizing::new(vec![b'x'; 128 * 1024]),
    128 * 1024,
    Duration::from_secs(2),
  )
  .await
  .unwrap();
  assert!(output.success);
  assert!(output.input_result.is_ok());
  assert_eq!(output.bytes.len(), 128 * 1024);
}

#[tokio::test]
async fn output_limits_terminate_the_helper() {
  let error = exchange(
    command("cat >/dev/null; printf '%s' 'too-long'; exec sleep 30"),
    "--credential-request",
    Zeroizing::new(vec![]),
    3,
    Duration::from_secs(2),
  )
  .await
  .err()
  .unwrap();
  assert_eq!(error.code, "credential_helper_invalid_response");
}

struct Fixture {
  path: std::path::PathBuf,
}

impl Fixture {
  fn new() -> Self {
    Self {
      path: std::env::temp_dir().join(format!(
        "ctl-credential-helper-{}.pid",
        uuid::Uuid::new_v4()
      )),
    }
  }

  fn command(&self) -> Command {
    let mut fixture = command("printf '%s' \"$$\" > \"$CREDENTIAL_FIXTURE_PID\"; exec sleep 30");
    fixture.env("CREDENTIAL_FIXTURE_PID", &self.path);
    fixture
  }

  async fn pid(&self) -> rustix::process::Pid {
    tokio::time::timeout(Duration::from_secs(2), async {
      loop {
        if let Ok(value) = std::fs::read_to_string(&self.path)
          && let Ok(pid) = value.parse::<i32>()
          && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
          return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap()
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.path);
  }
}

async fn assert_reaped(pid: rustix::process::Pid) {
  tokio::time::timeout(Duration::from_secs(2), async {
    loop {
      if rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH) {
        return;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("helper must be terminated and reaped");
}

#[tokio::test]
async fn timed_out_helper_is_terminated_and_reaped() {
  let fixture = Fixture::new();
  let command = fixture.command();
  let operation = tokio::spawn(exchange(
    command,
    "--credential-request",
    Zeroizing::new(vec![]),
    1024,
    Duration::from_millis(250),
  ));
  let pid = fixture.pid().await;
  let error = operation.await.unwrap().err().unwrap();
  assert_eq!(error.code, "credential_helper_timeout");
  assert_reaped(pid).await;
}

#[tokio::test]
async fn canceled_caller_terminates_and_reaps_helper() {
  let fixture = Fixture::new();
  let operation = tokio::spawn(exchange(
    fixture.command(),
    "--credential-request",
    Zeroizing::new(vec![]),
    1024,
    Duration::from_secs(30),
  ));
  let pid = fixture.pid().await;
  operation.abort();
  assert!(matches!(operation.await, Err(error) if error.is_cancelled()));
  assert_reaped(pid).await;
}

#[tokio::test]
async fn draining_after_dropped_exchange_waits_for_child_reaping() {
  let fixture = Fixture::new();
  let pid = {
    let operation = exchange(
      fixture.command(),
      "--credential-request",
      Zeroizing::new(vec![]),
      1024,
      Duration::from_secs(30),
    );
    tokio::pin!(operation);
    tokio::select! {
      pid = fixture.pid() => pid,
      _ = &mut operation => panic!("fixture helper must remain active"),
    }
  };
  tokio::time::timeout(Duration::from_secs(2), drain_exchanges())
    .await
    .expect("cancelled helpers must finish cleanup before shutdown")
    .unwrap();
  assert_eq!(
    rustix::process::test_kill_process(pid),
    Err(rustix::io::Errno::SRCH),
    "drain must finish only after the child has been terminated and reaped",
  );
}
