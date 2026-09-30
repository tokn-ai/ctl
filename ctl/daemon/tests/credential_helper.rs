use ctld_ipc::credentials::{MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Response};
use std::io::Write as _;
use std::process::{Command, Stdio};

fn request(bytes: &[u8]) -> std::process::Output {
  let socket =
    std::env::temp_dir().join(format!("credential-helper-{}.sock", uuid::Uuid::new_v4()));
  let mut child = Command::new(env!("CARGO_BIN_EXE_ctld"))
    .arg("--credential-request")
    .env_remove("CTLD_ASKPASS")
    .env("CTLD_SOCKET_PATH", &socket)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(bytes).unwrap();
  let output = child.wait_with_output().unwrap();
  assert!(!socket.exists(), "one-shot helper started a daemon socket");
  output
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
    assert!(output.stderr.is_empty());
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
