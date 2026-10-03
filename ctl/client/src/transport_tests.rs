use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(unix)]
#[tokio::test]
async fn fresh_proxy_routes_propagate_preparation_failure_before_starting_ssh() {
  use std::os::unix::fs::PermissionsExt as _;
  struct Cleanup(PathBuf);
  impl Drop for Cleanup {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }
  let directory = std::env::temp_dir().join(format!("ctl-proxy-provider-{}", uuid::Uuid::new_v4()));
  std::fs::create_dir(&directory).unwrap();
  std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
  let _cleanup = Cleanup(directory.clone());
  let output = timeout(
    TEST_TIMEOUT,
    Command::new(std::env::current_exe().unwrap())
      .args([
        "--exact",
        "transport_tests::proxy_preparation_child",
        "--nocapture",
      ])
      .env("CTL_PROXY_PROVIDER_TEST", "true")
      .env("HOME", &directory)
      .env_remove("CTLD_BIN")
      .kill_on_drop(true)
      .output(),
  )
  .await
  .unwrap()
  .unwrap();
  assert!(output.status.success(), "{output:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn proxy_preparation_child() {
  use std::sync::atomic::AtomicUsize;
  static CALLS: AtomicUsize = AtomicUsize::new(0);
  fn failing_provider() -> ctl_ipc::DaemonExecutableFuture {
    CALLS.fetch_add(1, Ordering::Relaxed);
    Box::pin(async {
      Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "bundled signature rejected",
      ))
    })
  }
  if std::env::var_os("CTL_PROXY_PROVIDER_TEST").is_none() {
    return;
  }
  let rejected = |error| {
    assert!(!is_retryable_connection_error(&error));
    assert!(
      matches!(error, CoreError::LocalConnection(ctl_ipc::ConnectError::PrepareDaemon(source)) if source.kind() == io::ErrorKind::PermissionDenied)
    );
  };
  ctl_ipc::register_daemon_executable_provider(failing_provider).unwrap();
  let mut options = SshConnectionOptions {
    gateways: vec![loopback_gateway(1080)],
    ..SshConnectionOptions::default()
  };
  prepare_ssh_base_arguments("fixture", &options, &SshInteraction::Inherit)
    .await
    .unwrap();
  options.gateways[0].kind = ctl_ipc::GatewayKind::Socks5;
  let multiplexed = SshInteraction::Multiplexed {
    control_path: PathBuf::from("/tmp/absent-master"),
  };
  let arguments = prepare_ssh_base_arguments("fixture", &options, &multiplexed)
    .await
    .unwrap();
  assert!(
    !arguments
      .iter()
      .any(|argument| argument.to_string_lossy().starts_with("ProxyCommand="))
  );
  assert_eq!(CALLS.load(Ordering::Relaxed), 0);

  rejected(
    open_ssh_service_interactive(
      "fixture",
      &options,
      &SshInteraction::Inherit,
      RemoteService::Ctmux,
    )
    .await
    .err()
    .unwrap(),
  );
  rejected(
    open_identified_ssh_service(
      "fixture",
      &options,
      &SshInteraction::Inherit,
      RemoteService::Task,
    )
    .await
    .err()
    .unwrap(),
  );
  rejected(
    open_identified_ssh_service_after_authentication(
      "fixture",
      &options,
      &SshInteraction::Inherit,
      RemoteService::Ctmux,
      ready(()),
    )
    .await
    .err()
    .unwrap(),
  );
  rejected(
    ssh_command_interactive("fixture", &options, &SshInteraction::Inherit, "true")
      .await
      .unwrap_err(),
  );
  rejected(
    install_ssh_unix_agent_interactive("fixture", &options, &SshInteraction::Inherit, "test", &[])
      .await
      .unwrap_err(),
  );
  assert_eq!(CALLS.load(Ordering::Relaxed), 5);
}

#[cfg(unix)]
fn loopback_gateway(port: u16) -> SshGateway {
  SshGateway {
    kind: ctl_ipc::GatewayKind::Ssh,
    vpn: None,
    destination: "127.0.0.1".into(),
    hostname: None,
    user: None,
    port: Some(port),
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  }
}

#[cfg(unix)]
async fn multiplexed_ssh_command(options: &SshConnectionOptions, control_path: PathBuf) -> Command {
  let mut command = Command::new(SSH_PROGRAM);
  // Isolate the real OpenSSH client from personal configuration and credentials.
  command.args(["-F", "/dev/null", "-o", "ConnectTimeout=1"]);
  let interaction = SshInteraction::Multiplexed { control_path };
  let extra = configure_ssh_interaction(&mut command, &interaction);
  command
    .args(extra)
    .args(
      prepare_ssh_base_arguments("fixture", options, &interaction)
        .await
        .unwrap(),
    )
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  command
}

#[cfg(unix)]
#[tokio::test]
async fn multiplexed_missing_master_never_contacts_the_host_or_gateway() {
  for gateway_kind in [
    None,
    Some(ctl_ipc::GatewayKind::Ssh),
    Some(ctl_ipc::GatewayKind::Socks5),
  ] {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let options = SshConnectionOptions {
      hostname: Some("127.0.0.1".into()),
      port: Some(port),
      gateways: gateway_kind
        .map(|kind| {
          let mut gateway = loopback_gateway(port);
          gateway.kind = kind;
          vec![gateway]
        })
        .unwrap_or_default(),
      ..SshConnectionOptions::default()
    };
    let path = PathBuf::from(format!("/tmp/ctl-mux-{}", uuid::Uuid::new_v4().simple()));
    assert!(!path.exists());
    let mut command = multiplexed_ssh_command(&options, path).await;
    command.arg("true");
    let output = tokio::select! {
      biased;
      accepted = listener.accept() => {
        drop(accepted);
        panic!("missing master attempted a fresh SSH connection (gateway={gateway_kind:?})");
      }
      output = timeout(TEST_TIMEOUT, command.output()) => output.unwrap().unwrap(),
    };
    // ProxyCommand=false may report a closed connection or a broken pipe,
    // depending on whether its exit races with OpenSSH writing its banner.
    // The listener above checks the no-fallback guarantee directly.
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(255), "{diagnostics}");
    assert!(!diagnostics.contains("Cannot specify -J with ProxyCommand"));
  }
}

#[cfg(unix)]
#[tokio::test]
async fn openssh_gateway_options_preserve_master_only_precedence() {
  let options = SshConnectionOptions {
    hostname: Some("127.0.0.1".into()),
    gateways: vec![loopback_gateway(2222)],
    ..SshConnectionOptions::default()
  };
  for multiplexed in [false, true] {
    let mut command = Command::new(SSH_PROGRAM);
    command.args(["-F", "/dev/null", "-G"]);
    if multiplexed {
      let extra = configure_ssh_interaction(
        &mut command,
        &SshInteraction::Multiplexed {
          control_path: PathBuf::from("/tmp/ctl-mux-config-test"),
        },
      );
      command.args(extra);
    }
    command.args(
      prepare_ssh_base_arguments("fixture", &options, &SshInteraction::Inherit)
        .await
        .unwrap(),
    );
    let output = command.output().await.unwrap();
    assert!(
      output.status.success(),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
    let config = String::from_utf8(output.stdout).unwrap();
    if multiplexed {
      assert!(config.lines().any(|line| line == "proxycommand false"));
      assert!(!config.lines().any(|line| line.starts_with("proxyjump ")));
    } else {
      assert!(config.lines().any(|line| matches!(
        line,
        "proxyjump 127.0.0.1:2222" | "proxyjump [127.0.0.1]:2222"
      )));
      assert!(!config.lines().any(|line| line.starts_with("proxycommand ")));
    }
  }
}

// Exercise real OS process pipes, including Windows binary stdio. These
// fixtures model the SSH child's stream boundary, not SSH authentication.
fn fixture(unix: &str, windows_script: &str) -> Command {
  #[cfg(unix)]
  {
    let _ = windows_script;
    let mut command = Command::new("sh");
    command.args(["-c", unix]);
    command
  }
  #[cfg(windows)]
  {
    let _ = unix;
    let mut command = Command::new("powershell.exe");
    command.args([
      "-NoLogo",
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
    ]);
    // Keep stdin exclusively for the binary protocol. Windows PowerShell
    // command mode can consume redirected input before the fixture runs.
    command.arg(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(windows_script),
    );
    command
  }
}

#[tokio::test]
async fn transport_consumes_marker_and_preserves_binary_io() {
  timeout(TEST_TIMEOUT, async {
    let mut command = fixture(
      "printf '%s' \"$CTL_TEST_STARTUP_NOISE\"; printf 'ctl-ssh-v1\n'; cat",
      "echo-transport.ps1",
    );
    command.env(
      "CTL_TEST_STARTUP_NOISE",
      "\u{1b}[32mWelcome\u{1b}[0m\nno final newline: ",
    );
    let mut transport = start_ssh_transport(command).await.unwrap();
    let payload = [0, 255, 128, b'\r', b'\n', 27, 1, b'x'];
    transport.write_all(&payload).await.unwrap();
    transport.flush().await.unwrap();
    let mut response = [0; 8];
    transport.read_exact(&mut response).await.unwrap();
    assert_eq!(response, payload);
  })
  .await
  .expect("process-pipe round trip timed out");
}

#[tokio::test]
async fn startup_reports_missing_markers_and_retains_stderr() {
  timeout(TEST_TIMEOUT, async {
    let noisy = fixture(
      "printf 'unexpected startup output\n'; printf 'remote wrapper failed\n' >&2",
      "noisy-transport.ps1",
    );
    let error = start_ssh_transport(noisy).await.err().unwrap();
    assert!(matches!(error, CoreError::InvalidSshPreface(_)));
    assert!(error.to_string().contains("unexpected startup output"));
    assert!(error.to_string().contains("remote wrapper failed"));
    assert!(!is_retryable_connection_error(&error));
    let failed = fixture(
      "printf 'Host key verification failed.\n' >&2; exit 255",
      "failed-transport.ps1",
    );
    let Err(CoreError::SshStartup(message)) = start_ssh_transport(failed).await else {
      panic!("expected SSH startup diagnostics");
    };
    assert!(message.contains("Host key verification failed."));
  })
  .await
  .expect("startup failure handling timed out");
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_startup_reaps_the_ssh_child_without_waiting_for_readiness() {
  let path = std::env::temp_dir().join(format!("ctl-startup-pid-{}", uuid::Uuid::new_v4()));
  let mut command = Command::new("sh");
  command
    .args([
      "-c",
      "printf '%s' \"$$\" > \"$CTL_TEST_CHILD_PID\"; printf 'banner'; read -r response",
    ])
    .env("CTL_TEST_CHILD_PID", &path);
  let task = tokio::spawn(start_ssh_transport(command));
  let pid = timeout(Duration::from_secs(5), async {
    loop {
      if let Ok(pid) = std::fs::read_to_string(&path)
        && !pid.is_empty()
      {
        break pid;
      }
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("child did not start");
  task.abort();
  assert!(task.await.err().unwrap().is_cancelled());
  timeout(Duration::from_secs(5), async {
    while Command::new("kill")
      .args(["-0", &pid])
      .stderr(Stdio::null())
      .status()
      .await
      .unwrap()
      .success()
    {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .expect("cancelled startup left its child running");
  std::fs::remove_file(path).unwrap();
}

// Native Windows CI must also exercise the installed OpenSSH executable,
// without credentials, a real remote account, or host-key policy changes.
#[cfg(windows)]
#[tokio::test]
async fn windows_openssh_reports_connection_failure() {
  timeout(TEST_TIMEOUT, async {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let options = SshConnectionOptions {
      hostname: Some("127.0.0.1".into()),
      port: Some(listener.local_addr().unwrap().port()),
      ..SshConnectionOptions::default()
    };
    let server = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      stream.write_all(b"SSH-2.0-ctl-test\r\n").await.unwrap();
      let mut buffer = [0; 1024];
      let _ = stream.read(&mut buffer).await;
      // Close before key exchange; no authentication can occur.
    });
    let result = open_ssh_tunnel_interactive("fixture", &options, &SshInteraction::Batch).await;
    assert!(matches!(result, Err(CoreError::SshStartup(_))));
    server.await.unwrap();
  })
  .await
  .expect("Windows OpenSSH startup timed out");
}

#[tokio::test]
async fn identified_transport_consumes_metadata_and_preserves_binary_io() {
  timeout(TEST_TIMEOUT, async {
    let identity = ctl_proto::RemoteIdentity {
      remote_id: uuid::Uuid::new_v4().to_string(),
      agent_version: "0.1.0".into(),
      build: None,
      ctmux_restart_supported: false,
      bundle: None,
    };
    let json = serde_json::to_string(&identity).unwrap();
    let mut command = identified_fixture(&json);
    command
      .env("CTL_TEST_STARTUP_NOISE", "Welcome\nwithout final newline: ")
      .env("CTL_TEST_AUTHENTICATION_MARKER", "true");
    let authenticated = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&authenticated);
    let mut transport = start_ssh_transport_identified(command, true, true, async move {
      observed.store(true, Ordering::SeqCst);
    })
    .await
    .unwrap();
    assert!(authenticated.load(Ordering::SeqCst));
    assert_eq!(transport.remote_identity.as_deref(), Some(&identity));
    let payload = [0, 255, 128, b'\r', b'\n', 27, 1, b'x'];
    transport.write_all(&payload).await.unwrap();
    transport.flush().await.unwrap();
    let mut response = [0; 8];
    transport.read_exact(&mut response).await.unwrap();
    assert_eq!(response, payload);
  })
  .await
  .unwrap();
}

fn identified_fixture(json: &str) -> Command {
  use std::fmt::Write as _;
  let size = u32::try_from(json.len())
    .unwrap()
    .to_be_bytes()
    .iter()
    .fold(String::new(), |mut out, byte| {
      write!(out, "\\{byte:03o}").unwrap();
      out
    });
  let mut command = fixture(
    "if [ -n \"$CTL_TEST_CHILD_PID\" ]; then printf '%s' \"$$\" > \"$CTL_TEST_CHILD_PID\"; fi; printf '%s' \"$CTL_TEST_STARTUP_NOISE\"; if [ -n \"$CTL_TEST_AUTHENTICATION_MARKER\" ]; then printf 'ctl-ssh-auth-v1\n'; fi; printf '%s\n' \"$CTL_TEST_IDENTITY_MARKER\"; printf '%b' \"$CTL_TEST_IDENTITY_SIZE\"; printf '%s' \"$CTL_TEST_IDENTITY_JSON\"; if [ -n \"$CTL_TEST_SERVICE_INPUT\" ]; then exec cat > \"$CTL_TEST_SERVICE_INPUT\"; fi; exec cat",
    "identified-transport.ps1",
  );
  command
    .env("CTL_TEST_IDENTITY_SIZE", size)
    .env("CTL_TEST_IDENTITY_JSON", json)
    .env("CTL_TEST_IDENTITY_MARKER", "ctl-ssh-v3")
    .env("CTL_TEST_STARTUP_NOISE", "")
    .env("CTL_TEST_AUTHENTICATION_MARKER", "")
    .env("CTL_TEST_CHILD_PID", "")
    .env("CTL_TEST_SERVICE_INPUT", "");
  command
}

#[tokio::test]
async fn legacy_inspection_reads_only_identity_after_startup_noise() {
  timeout(TEST_TIMEOUT, async {
    let json = serde_json::json!({
      "remote_id": uuid::Uuid::new_v4().to_string(),
      "agent_version": "0.1.0",
      "rmux_restart_supported": true,
      "bundle": {
        "app_version": "0.1.0",
        "bundle_id": "0.1.0-dev.41d2f11",
        "git_revision": "41d2f11",
        "target_triple": "x86_64-unknown-linux-musl"
      }
    });
    let mut command = identified_fixture(&json.to_string());
    command.env("CTL_TEST_IDENTITY_MARKER", "ctl-ssh-v2").env(
      "CTL_TEST_STARTUP_NOISE",
      "Welcome\nprofile without newline: ",
    );
    let identity = inspect_legacy_ssh_command(command).await.unwrap();
    assert_eq!(identity.remote_id, json["remote_id"]);
    assert_eq!(identity.bundle.unwrap().bundle_id, "0.1.0-dev.41d2f11");
    assert!(!identity.ctmux_restart_supported);
    assert!(identity.build.is_none());
  })
  .await
  .expect("legacy inspection must close without waiting for service frames");
}

#[tokio::test]
async fn legacy_inspection_rejects_invalid_and_oversized_metadata() {
  timeout(TEST_TIMEOUT, async {
    for json in ["{}".to_owned(), " ".repeat(8193)] {
      let mut command = identified_fixture(&json);
      command.env("CTL_TEST_IDENTITY_MARKER", "ctl-ssh-v2");
      assert!(matches!(
        inspect_legacy_ssh_command(command).await,
        Err(CoreError::RemoteIdentity(_))
      ));
    }
  })
  .await
  .expect("invalid legacy metadata must terminate its producer");
}

#[tokio::test]
async fn legacy_inspection_rejects_other_reserved_markers() {
  timeout(TEST_TIMEOUT, async {
    for marker in ["ctl-ssh-v3", "ctl-ssh-v99", "ctl-ssh-nf"] {
      let mut command = identified_fixture("{}");
      command.env("CTL_TEST_IDENTITY_MARKER", marker);
      assert!(matches!(
        inspect_legacy_ssh_command(command).await,
        Err(CoreError::UnsupportedSshProtocol { marker: received }) if received == marker
      ));
    }
  })
  .await
  .expect("only v2 is permitted for legacy inspection");
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_inspection_reaps_its_child_without_sending_service_input() {
  let directory = std::env::temp_dir().join(format!(
    "ctl-legacy-inspection-{}",
    uuid::Uuid::new_v4().simple()
  ));
  std::fs::create_dir(&directory).unwrap();
  let json = serde_json::json!({
    "remote_id": uuid::Uuid::new_v4().to_string(),
    "agent_version": "0.1.0"
  });
  for (index, metadata) in [json.to_string(), "{}".into()].into_iter().enumerate() {
    let pid_path = directory.join(format!("pid-{index}"));
    let input_path = directory.join(format!("input-{index}"));
    let mut command = identified_fixture(&metadata);
    command
      .env("CTL_TEST_IDENTITY_MARKER", "ctl-ssh-v2")
      .env("CTL_TEST_CHILD_PID", &pid_path)
      .env("CTL_TEST_SERVICE_INPUT", &input_path);
    let result = timeout(TEST_TIMEOUT, inspect_legacy_ssh_command(command))
      .await
      .unwrap();
    assert_eq!(result.is_ok(), index == 0);
    let pid = std::fs::read_to_string(pid_path).unwrap();
    assert!(
      !Command::new("kill")
        .args(["-0", &pid])
        .stderr(Stdio::null())
        .status()
        .await
        .unwrap()
        .success(),
      "legacy metadata producer was not reaped"
    );
    // The producer can be killed before creating the sink. If it did create it,
    // the compatibility probe must still have sent no service bytes.
    if input_path.exists() {
      assert_eq!(std::fs::read(input_path).unwrap(), [] as [u8; 0]);
    }
  }
  std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unsupported_transport_versions_offer_an_update_instead_of_blaming_the_shell() {
  timeout(TEST_TIMEOUT, async {
    for marker in ["ctl-ssh-v2", "ctl-ssh-v99"] {
      let mut command = fixture(
        "printf '%s\n' \"$CTL_TEST_TRANSPORT_MARKER\"; cat",
        "echo-transport.ps1",
      );
      command
        .env("CTL_TEST_TRANSPORT_MARKER", marker)
        .env("CTL_TEST_STARTUP_NOISE", "");
      let error = start_ssh_transport_identified(command, true, false, ready(()))
        .await
        .err()
        .unwrap();
      assert!(matches!(&error, CoreError::UnsupportedSshProtocol { marker: received } if received == marker));
      assert!(error.to_string().contains("update the remote components"));
      assert!(!is_retryable_connection_error(&error));
    }
  })
  .await
  .expect("unsupported markers must fail before waiting for protocol input");
}

#[tokio::test]
async fn authentication_hook_does_not_run_when_the_wrapper_skips_its_marker() {
  let identity = serde_json::json!({
    "remote_id": uuid::Uuid::new_v4().to_string(),
    "agent_version": "0.1.0",
  });
  let command = identified_fixture(&identity.to_string());
  let authenticated = Arc::new(AtomicBool::new(false));
  let observed = Arc::clone(&authenticated);
  let result = timeout(
    TEST_TIMEOUT,
    start_ssh_transport_identified(command, true, true, async move {
      observed.store(true, Ordering::SeqCst);
    }),
  )
  .await
  .unwrap();
  assert!(!authenticated.load(Ordering::SeqCst));
  let error = result.err().unwrap();
  assert!(matches!(error, CoreError::InvalidSshPreface(_)));
  assert!(
    error
      .to_string()
      .contains("skipped the SSH authentication marker")
  );
}

#[tokio::test]
async fn identified_transport_rejects_old_agents_and_invalid_metadata() {
  timeout(TEST_TIMEOUT, async {
    let legacy = fixture("printf 'ctl-ssh-v1\n'; cat", "echo-transport.ps1");
    assert!(matches!(
      start_ssh_transport_identified(legacy, true, false, ready(())).await,
      Err(CoreError::IdentityUnsupported)
    ));
    let malformed = identified_fixture("{}");
    assert!(matches!(
      start_ssh_transport_identified(malformed, true, false, ready(())).await,
      Err(CoreError::RemoteIdentity(_))
    ));
  })
  .await
  .unwrap();
}

#[tokio::test]
async fn authentication_hook_runs_when_the_remote_agent_is_missing() {
  timeout(TEST_TIMEOUT, async {
    let command = fixture(
      "printf 'ctl-ssh-auth-v1\nctl-ssh-nf\n'; printf 'bash: ctl-agent: 未找到\n' >&2; exit 127",
      "authenticated-missing-agent.ps1",
    );
    let authenticated = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&authenticated);
    let result = start_ssh_transport_identified(command, true, true, async move {
      observed.store(true, Ordering::SeqCst);
    })
    .await;

    assert!(authenticated.load(Ordering::SeqCst));
    assert!(matches!(result, Err(CoreError::AgentNotFound)));
  })
  .await
  .expect("authenticated missing-agent handling timed out");
}

#[tokio::test]
async fn transport_recognizes_the_missing_agent_protocol_marker() {
  timeout(TEST_TIMEOUT, async {
    let command = fixture(
      "printf 'ctl-ssh-nf\n'; printf 'bash: ctl-agent: 未找到\n' >&2; exit 127",
      "missing-agent.ps1",
    );

    assert!(matches!(
      start_ssh_transport(command).await,
      Err(CoreError::AgentNotFound)
    ));
  })
  .await
  .expect("missing-agent handling timed out");
}

#[cfg(unix)]
#[tokio::test]
async fn fixed_command_closes_stdin_before_waiting_for_response() {
  for input in [
    b"".as_slice(),
    b"{\"expected_remote_id\":\"test\"}".as_slice(),
  ] {
    let mut command = Command::new("sh");
    command.args([
      "-c",
      "printf 'ctl-command-v1\n'; cat; printf '\nrequest-complete'",
    ]);
    let output = timeout(
      Duration::from_secs(2),
      run_marked_fixed_command(command, input, b"ctl-command-v1\n"),
    )
    .await
    .expect("command must receive EOF before the caller waits for output")
    .expect("command should succeed");
    assert_eq!(output, [input, b"\nrequest-complete"].concat());
  }
}

#[cfg(unix)]
#[tokio::test]
async fn fixed_command_strips_startup_noise_but_keeps_response_strict_and_reports_ssh_failures() {
  for (marker, response) in [
    ("ctl-platform-v1\n", "Linux\nx86_64\n"),
    ("ctl-command-v1\n", "{\"terminated_sessions\":0}\n"),
  ] {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'banner without newline'; printf '%s%s' \"$CTL_TEST_MARKER\" \"$CTL_TEST_RESPONSE\""])
      .env("CTL_TEST_MARKER", marker)
      .env("CTL_TEST_RESPONSE", response);
    let output = run_marked_fixed_command(command, &[], marker.as_bytes())
      .await
      .unwrap();
    assert_eq!(output, response.as_bytes());
  }
  let mut failed = Command::new("sh");
  failed.args([
    "-c",
    "printf 'Host key verification failed.\n' >&2; exit 255",
  ]);
  let error = run_marked_fixed_command(failed, &[], b"ctl-command-v1\n")
    .await
    .unwrap_err();
  assert!(matches!(error, CoreError::SshCommandFailed { .. }));
  assert!(error.to_string().contains("Host key verification failed."));
}
