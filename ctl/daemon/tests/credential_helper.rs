use ctl_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Response};
use std::io::Write as _;
use std::process::{Command, Stdio};

fn request(bytes: &[u8]) -> std::process::Output {
  request_with_fixture(bytes, cfg!(windows))
}

fn request_with_fixture(bytes: &[u8], isolated_child: bool) -> std::process::Output {
  let directory = std::env::temp_dir().join(format!("credential-history-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir(&directory).unwrap();
  let socket =
    std::env::temp_dir().join(format!("credential-helper-{}.sock", uuid::Uuid::new_v4()));
  let mut command = if isolated_child {
    // The Windows home-directory API ignores HOME. Run the same helper code in
    // a test child with an explicit store instead of writing the real account's
    // shared history. Unix retains the actual ctld entry-point smoke coverage.
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
      .args(["--exact", "credential_helper_child", "--nocapture"])
      .env("CTL_CREDENTIAL_HELPER_FIXTURE", &directory);
    command
  } else {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ctld"));
    command.arg("--credential-request").env("HOME", &directory);
    command
  };
  let mut child = command
    .env_remove("CTLD_ASKPASS")
    .env_remove("CTLD_IDENTITY_ASKPASS")
    .env_remove("CTL_LOG_LEVEL")
    .env("CTLD_SOCKET_PATH", &socket)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(bytes).unwrap();
  let mut output = child.wait_with_output().unwrap();
  if isolated_child {
    assert!(output.status.success(), "{output:?}");
    // The Rust test runner owns child stdout. Keep the protocol response in its
    // own file, while retaining exit status and strict captured stderr checks.
    output.stdout = std::fs::read(directory.join("response.json")).unwrap();
    if serde_json::from_slice::<ctl_ipc::credentials::Request>(bytes).is_ok() {
      let history = ctl_core::observability::Store::new(directory.join("history"))
        .read(ctl_core::observability::Stream::Audit, 10, false)
        .unwrap();
      assert!(history.complete);
      assert_eq!(
        history.records.len(),
        2,
        "request was recorded in its isolated store"
      );
    }
  }
  assert!(!socket.exists(), "one-shot helper started a daemon socket");
  std::fs::remove_dir_all(directory).unwrap();
  output
}

#[test]
fn credential_helper_child() {
  let Some(directory) = std::env::var_os("CTL_CREDENTIAL_HELPER_FIXTURE") else {
    return;
  };
  let directory = std::path::PathBuf::from(directory);
  ctl_core::test_fixtures::initialize_history(directory.join("history")).unwrap();
  let response = std::fs::File::create(directory.join("response.json")).unwrap();
  ctld::credentials::run(std::io::stdin().lock(), response).unwrap();
}

#[test]
fn isolated_helpers_preserve_the_protocol_during_parallel_recording() {
  std::thread::scope(|scope| {
    let children: Vec<_> = (0..2)
      .map(|_| {
        scope.spawn(|| {
          request_with_fixture(
            br#"{"type":"forget","credential_id":"not-an-owned-item"}"#,
            true,
          )
        })
      })
      .collect();
    for child in children {
      let output = child.join().unwrap();
      assert!(output.status.success(), "{output:?}");
      assert!(output.stderr.is_empty(), "{output:?}");
      assert!(output.stdout.len() <= MAX_RESPONSE_BYTES);
      assert!(matches!(
        serde_json::from_slice::<Response>(&output.stdout).unwrap(),
        Response::Error { code, .. } if code == "credential_request_invalid"
      ));
    }
  });
}

#[test]
fn invalid_request_returns_only_a_bounded_error_without_starting_a_daemon() {
  for bytes in [
    br#"{"type":"forget","credential_id":"not-an-owned-item"}"#.to_vec(),
    br#"{"type":"list","password":"private-input-canary"}"#.to_vec(),
    vec![b'x'; MAX_REQUEST_BYTES + 1],
  ] {
    // All fixtures are rejected before touching any real Keychain item.
    let output = request(&bytes);
    assert!(output.status.success());
    assert_eq!(output.stderr, Vec::<u8>::new());
    assert!(output.stdout.len() <= MAX_RESPONSE_BYTES);
    let response: Response = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
      matches!(response, Response::Error { code, .. } if code == "credential_request_invalid")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-input-canary"));
  }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_reports_explicitly_without_starting_a_daemon() {
  let output = request(br#"{"type":"list"}"#);
  assert!(output.status.success());
  let response: Response = serde_json::from_slice(&output.stdout).unwrap();
  assert!(
    matches!(response, Response::Error { code, .. } if code == "credential_store_unsupported")
  );
}
