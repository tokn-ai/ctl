use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[test]
fn caches_passwords_but_not_host_trust_or_one_time_codes() {
  assert!(cacheable_prompt("rmux@host's password:"));
  assert!(cacheable_prompt("Enter passphrase for key '/key':"));
  assert!(!cacheable_prompt("Verification code:"));
  assert!(!cacheable_prompt(
    "Are you sure you want to continue connecting?"
  ));
}

fn prompt_context() -> (PromptContext, mpsc::UnboundedReceiver<serde_json::Value>) {
  let (sender, receiver) = mpsc::unbounded_channel();
  let channel = Channel::new(move |body| {
    let _ = sender.send(body.deserialize::<serde_json::Value>().unwrap());
    Ok(())
  });
  let (cancel, _) = watch::channel(false);
  (
    PromptContext {
      attempt: Arc::new(Attempt {
        cancel,
        responses: Mutex::default(),
      }),
      channel,
    },
    receiver,
  )
}

async fn request(socket: PathBuf, token: String, confirm: bool) -> Option<String> {
  let mut stream = UnixStream::connect(socket).await.unwrap();
  let request = HelperRequest {
    token,
    message: "test@host's password:".into(),
    confirm,
  };
  let mut encoded = serde_json::to_vec(&request).unwrap();
  encoded.push(b'\n');
  stream.write_all(&encoded).await.unwrap();
  let mut line = String::new();
  timeout(
    Duration::from_secs(5),
    BufReader::new(stream).read_line(&mut line),
  )
  .await
  .unwrap()
  .unwrap();
  if line.is_empty() {
    None
  } else {
    serde_json::from_str(&line).unwrap()
  }
}

#[tokio::test]
async fn bridge_is_private_checks_capabilities_and_removes_its_socket() {
  let secrets = Secrets::default();
  secrets.lock().unwrap().insert(
    "test@host's password:".into(),
    Zeroizing::new("synthetic-secret".into()),
  );
  let bridge = Bridge::start(secrets, None).unwrap();
  let directory = bridge.directory.clone();
  let socket = bridge.socket.clone();
  assert_eq!(
    std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
    0o700
  );
  assert!(socket.as_os_str().len() < 104);
  assert!(
    request(socket.clone(), "wrong-capability".into(), false)
      .await
      .is_none()
  );
  assert_eq!(
    request(socket.clone(), bridge.token.clone(), false)
      .await
      .as_deref(),
    Some("synthetic-secret")
  );
  // Confirmation must never replay a cached secret, even with identical text.
  assert!(
    request(socket.clone(), bridge.token.clone(), true)
      .await
      .is_none()
  );
  drop(bridge);
  assert!(!socket.exists());
  assert!(!directory.exists());
}

#[tokio::test]
async fn prompt_responses_are_window_scoped_single_use_and_cancellable() {
  let (context, mut prompts) = prompt_context();
  let key = ("test-window".into(), uuid::Uuid::new_v4().to_string());
  registry()
    .lock()
    .unwrap()
    .attempts
    .insert(key.clone(), context.attempt.clone());
  let guard = AttemptGuard(key.clone());
  let mut cancelled = context.attempt.cancel.subscribe();
  let request = HelperRequest {
    token: String::new(),
    message: "Password:".into(),
    confirm: false,
  };
  let task = tokio::spawn(async move { ask(Some(&context), &request).await });
  let prompt = timeout(Duration::from_secs(5), prompts.recv())
    .await
    .unwrap()
    .unwrap();
  let prompt_id = prompt["prompt_id"].as_str().unwrap();
  assert_eq!(prompt["kind"], "secret");
  assert!(
    respond(
      "other-window",
      &key.1,
      prompt_id,
      Some("synthetic-secret".into())
    )
    .is_err()
  );
  assert!(respond(&key.0, &key.1, prompt_id, Some("invalid\nresponse".into())).is_err());
  respond(&key.0, &key.1, prompt_id, Some("synthetic-secret".into())).unwrap();
  assert!(respond(&key.0, &key.1, prompt_id, None).is_err());
  assert_eq!(task.await.unwrap().as_deref(), Some("synthetic-secret"));
  cancel_window(&key.0);
  cancelled.changed().await.unwrap();
  assert!(*cancelled.borrow());
  drop(guard);
  assert!(!registry().lock().unwrap().attempts.contains_key(&key));
}

#[tokio::test]
#[ignore = "requires RMUX_TEST_ASKPASS_PROGRAM pointing to the built rmux-app binary"]
async fn built_binary_delivers_a_secret_without_starting_tauri() {
  let secrets = Secrets::default();
  secrets.lock().unwrap().insert(
    "Password:".into(),
    Zeroizing::new("synthetic-secret".into()),
  );
  let bridge = Bridge::start(secrets.clone(), None).unwrap();
  let program = std::env::var("RMUX_TEST_ASKPASS_PROGRAM").unwrap();
  let mut command = Command::new(program);
  command
    .arg("Password:")
    .env("CTL_SSH_ASKPASS", "1")
    .env("CTL_SSH_ASKPASS_SOCKET", &bridge.socket)
    .env("CTL_SSH_ASKPASS_TOKEN", &bridge.token)
    .kill_on_drop(true);
  let output = timeout(Duration::from_secs(5), command.output())
    .await
    .unwrap()
    .unwrap();
  assert!(output.status.success());
  assert_eq!(output.stdout, b"synthetic-secret\n");
  assert!(output.stderr.is_empty());
  secrets.lock().unwrap().clear();
  // No cache or UI means cancellation and no stdout, never a graphical process.
  let output = timeout(Duration::from_secs(5), command.output())
    .await
    .unwrap()
    .unwrap();
  assert!(!output.status.success());
  assert!(output.stdout.is_empty());
  assert!(output.stderr.is_empty());
}

#[tokio::test]
#[ignore = "requires test container, built app, RMUX_TEST_SSH_IDENTITY and RMUX_TEST_SSH_FINGERPRINT"]
async fn openssh_host_verification_uses_the_prompt_bridge() {
  let program = std::env::var("RMUX_TEST_ASKPASS_PROGRAM").unwrap();
  let identity = std::env::var("RMUX_TEST_SSH_IDENTITY").unwrap();
  let fingerprint = std::env::var("RMUX_TEST_SSH_FINGERPRINT").unwrap();
  let (context, mut prompts) = prompt_context();
  let attempt = context.attempt.clone();
  let bridge = Bridge::start(Secrets::default(), Some(context)).unwrap();
  let known_hosts = bridge.directory.join("known_hosts");
  let mut child = Command::new("ssh")
    .args([
      "-F",
      "/dev/null",
      "-T",
      "-p",
      "2222",
      "-l",
      "rmux",
      "-i",
      &identity,
      "-o",
      "StrictHostKeyChecking=ask",
      "-o",
      "IdentitiesOnly=yes",
      "-o",
      "GlobalKnownHostsFile=/dev/null",
    ])
    .arg("-o")
    .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
    .args(["--", "127.0.0.1", "exec", "ctld", "connect"])
    .env("SSH_ASKPASS", program)
    .env("SSH_ASKPASS_REQUIRE", "force")
    .env("DISPLAY", "rmux-test")
    .env("CTL_SSH_ASKPASS", "1")
    .env("CTL_SSH_ASKPASS_SOCKET", &bridge.socket)
    .env("CTL_SSH_ASKPASS_TOKEN", &bridge.token)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
  let prompt = timeout(Duration::from_secs(10), prompts.recv())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(prompt["kind"], "confirm");
  assert!(prompt["message"].as_str().unwrap().contains(&fingerprint));
  attempt
    .responses
    .lock()
    .unwrap()
    .remove(prompt["prompt_id"].as_str().unwrap())
    .unwrap()
    .send(Some("yes".into()))
    .unwrap();
  let mut marker = [0_u8; 11];
  timeout(
    Duration::from_secs(10),
    child.stdout.as_mut().unwrap().read_exact(&mut marker),
  )
  .await
  .unwrap()
  .unwrap();
  assert_eq!(&marker, b"ctl-ssh-v1\n");
  let stream = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
  timeout(Duration::from_secs(10), verification::verify(stream))
    .await
    .unwrap()
    .unwrap();
  child.kill().await.unwrap();
  std::fs::remove_file(known_hosts).unwrap();
}
