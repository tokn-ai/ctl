#![cfg(unix)]

use ctld::identities::{IdentityError, LocalAgent, inspect_path};
use ctld_ipc::identities::{MAX_REQUEST_BYTES, Response};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use zeroize::Zeroizing;

struct Fixture(PathBuf);
impl Fixture {
  fn new() -> Self {
    let path = Path::new("/tmp").join(format!("ctld-key-test-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir(&path).unwrap();
    Self(path)
  }

  fn key(&self, name: &str, kind: &str, pem: bool) -> PathBuf {
    let path = self.0.join(name);
    // The passphrase is synthetic test data, never a user credential.
    let mut command = Command::new("/usr/bin/ssh-keygen");
    command
      .args(["-q", "-t", kind, "-N", "synthetic-test-passphrase", "-f"])
      .arg(&path);
    if pem {
      command.args(["-m", "PEM"]);
    }
    assert!(command.status().unwrap().success());
    path
  }
}
impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn helper_program() -> &'static Path {
  Path::new(env!("CARGO_BIN_EXE_ctld"))
}

#[tokio::test]
async fn unlock_verifies_real_synthetic_key_and_rejects_wrong_passphrase() {
  let fixture = Fixture::new();
  let path = fixture.key("fixture", "ed25519", false);
  let snapshot = inspect_path(path.to_str().unwrap()).unwrap();
  let mut agent = LocalAgent::start_with_program(helper_program())
    .await
    .unwrap();
  let socket = agent.socket_path().to_path_buf();
  assert!(matches!(
    agent
      .add_identity(&snapshot, Zeroizing::new("wrong-fixture-passphrase".into()))
      .await,
    Err(IdentityError::UnlockFailed)
  ));
  let verified = agent
    .add_identity(
      &snapshot,
      Zeroizing::new("synthetic-test-passphrase".into()),
    )
    .await
    .unwrap();
  assert_eq!(
    snapshot.public_key.as_deref(),
    Some(verified.public_key.as_str())
  );
  assert_eq!(verified.fingerprint, snapshot.fingerprint.unwrap());
  assert_eq!(verified.key_type, "ssh-ed25519");
  assert!(verified.public_key.starts_with("ssh-ed25519 "));
  drop(agent);
  assert!(!socket.exists());
}

#[tokio::test]
async fn pem_verification_cannot_select_another_loaded_key() {
  let fixture = Fixture::new();
  let first = fixture.key("first", "ed25519", false);
  let second = fixture.key("second", "rsa", true);
  let first = inspect_path(first.to_str().unwrap()).unwrap();
  let second = inspect_path(second.to_str().unwrap()).unwrap();
  assert!(second.fingerprint.is_none());
  let mut agent = LocalAgent::start_with_program(helper_program())
    .await
    .unwrap();
  let first_verified = agent
    .add_identity(&first, Zeroizing::new("synthetic-test-passphrase".into()))
    .await
    .unwrap();
  let second_verified = agent
    .add_identity(&second, Zeroizing::new("synthetic-test-passphrase".into()))
    .await
    .unwrap();
  assert_eq!(second_verified.key_type, "ssh-rsa");
  assert_ne!(first_verified.fingerprint, second_verified.fingerprint);
  // Rejected before Keychain access: a verification proof belongs to exactly
  // one file snapshot and exactly the passphrase that unlocked that snapshot.
  assert!(matches!(
    ctld::identities::save_verified(&second, &first_verified, "synthetic-test-passphrase"),
    Err(IdentityError::InvalidRequest)
  ));
  assert!(matches!(
    ctld::identities::save_verified(&second, &second_verified, "different-passphrase"),
    Err(IdentityError::InvalidRequest)
  ));
  let repeated = agent
    .add_identity(&first, Zeroizing::new("synthetic-test-passphrase".into()))
    .await
    .unwrap();
  assert_eq!(repeated.fingerprint, first_verified.fingerprint);
}

#[tokio::test]
async fn changed_file_is_rejected_before_import() {
  let fixture = Fixture::new();
  let path = fixture.key("fixture", "ed25519", false);
  let snapshot = inspect_path(path.to_str().unwrap()).unwrap();
  std::fs::write(&path, b"replaced").unwrap();
  let mut agent = LocalAgent::start_with_program(helper_program())
    .await
    .unwrap();
  assert!(matches!(
    agent
      .add_identity(
        &snapshot,
        Zeroizing::new("synthetic-test-passphrase".into())
      )
      .await,
    Err(IdentityError::FileChanged)
  ));
}

#[test]
fn rejected_requests_never_reach_keychain_or_echo_secret_fields() {
  for request in [
    br#"{"type":"forget","identity_id":"another-keychain-service"}"#.to_vec(),
    br#"{"type":"list","paths":[],"passphrase":"synthetic-secret-canary"}"#.to_vec(),
    vec![b'x'; MAX_REQUEST_BYTES + 1],
  ] {
    let mut child = Command::new(helper_program())
      .arg("--identity-request")
      .env_remove("CTLD_ASKPASS")
      .env_remove("CTLD_IDENTITY_ASKPASS")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-secret-canary"));
    assert!(
      matches!(serde_json::from_slice::<Response>(&output.stdout).unwrap(), Response::Error { code, .. } if code == "identity_invalid_request")
    );
  }
}

#[tokio::test]
async fn agent_exits_after_owner_is_killed_without_running_destructors() {
  let fixture = Fixture::new();
  let key = fixture.key("owner-key", "ed25519", false);
  let record = fixture.0.join("socket-path");
  let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
    .args(["--ignored", "--exact", "agent_owner_fixture"])
    .env("CTLD_TEST_IDENTITY_OWNER_RECORD", &record)
    .env("CTLD_TEST_IDENTITY_KEY", &key)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let socket = tokio::time::timeout(Duration::from_secs(8), async {
    loop {
      if let Ok(path) = std::fs::read_to_string(&record)
        && !path.is_empty()
      {
        break PathBuf::from(path);
      }
      assert!(
        child.try_wait().unwrap().is_none(),
        "owner fixture exited before loading its key"
      );
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .unwrap();
  assert!(socket.exists());
  child.kill().await.unwrap();
  tokio::time::timeout(Duration::from_secs(15), async {
    while socket.exists() {
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
  })
  .await
  .expect("agent survived its owner's abrupt death");
  std::fs::remove_dir(socket.parent().unwrap()).unwrap();
}

#[test]
#[ignore = "Subprocess fixture for abrupt-owner-death test"]
fn agent_owner_fixture() {
  let record = std::env::var_os("CTLD_TEST_IDENTITY_OWNER_RECORD").unwrap();
  let path = std::env::var("CTLD_TEST_IDENTITY_KEY").unwrap();
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap();
  let _agent = runtime.block_on(async {
    let snapshot = inspect_path(&path).unwrap();
    let mut agent = LocalAgent::start_with_program(helper_program())
      .await
      .unwrap();
    agent
      .add_identity(
        &snapshot,
        Zeroizing::new("synthetic-test-passphrase".into()),
      )
      .await
      .unwrap();
    std::fs::write(record, agent.socket_path().as_os_str().as_encoded_bytes()).unwrap();
    agent
  });
  loop {
    std::thread::park();
  }
}
