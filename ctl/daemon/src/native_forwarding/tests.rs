use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

fn target() -> SshTarget {
  SshTarget {
    destination: "configured-alias".into(),
    ssh_config_alias: Some("configured-alias".into()),
    use_ssh_config_master: Some(false),
    hostname: Some("127.0.0.1".into()),
    user: Some("developer".into()),
    port: Some(2222),
    identity_file: Some(std::env::temp_dir().join("private key")),
    gateways: vec![],
  }
}

fn forward(port: u16) -> LocalPortForward {
  LocalPortForward {
    forward_id: "native-forward".into(),
    bind_address: "127.0.0.1".into(),
    local_port: port,
    remote_host: "::1".into(),
    remote_port: 5432,
  }
}

fn arguments(command: &Command) -> Vec<String> {
  command
    .as_std()
    .get_args()
    .map(|argument| argument.to_string_lossy().into_owned())
    .collect()
}

#[test]
fn native_commands_preserve_config_aliases_and_override_only_owned_session_behavior() {
  let nonce = readiness_nonce();
  assert!(nonce.starts_with("ctld-forward-ready-"));
  assert!(
    nonce
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
  );
  let mut target = target();
  target.gateways.push(ctl_ipc::SshGateway {
    kind: ctl_ipc::GatewayKind::Ssh,
    vpn: None,
    destination: "gateway-alias".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctl_ipc::SshGatewayMode::Automatic,
  });
  for (command, mode) in [
    (anchor_command(&target, &nonce), "-N"),
    (channel_command(&target, &forward(15432), &nonce), "-W"),
  ] {
    let arguments = arguments(&command);
    assert!(arguments.contains(&mode.into()));
    assert!(
      !arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-F" | "-S" | "-M" | "-f"))
    );
    for option in [
      "ControlMaster=no",
      "ControlPath=none",
      "ControlPersist=no",
      "ForkAfterAuthentication=no",
      "RemoteCommand=none",
      "ClearAllForwardings=yes",
      "BatchMode=yes",
      "ForwardAgent=no",
      "ForwardX11=no",
      "Tunnel=no",
      "ProxyJump=gateway-alias",
      "HostName=127.0.0.1",
    ] {
      assert!(
        arguments.iter().any(|argument| argument == option),
        "missing {option}: {arguments:?}"
      );
    }
    assert!(arguments.contains(&format!("LocalCommand=echo {nonce}")));
    assert!(
      arguments.contains(
        &target
          .identity_file
          .as_ref()
          .unwrap()
          .to_string_lossy()
          .into_owned()
      )
    );
    assert_eq!(arguments.last().unwrap(), "configured-alias");
    if mode == "-W" {
      assert!(arguments.contains(&"[::1]:5432".into()));
    }
  }
}

#[tokio::test]
async fn readiness_strips_only_the_exact_nonce_and_preserves_binary_channel_bytes() {
  let nonce = readiness_nonce();
  let payload = b"\0\xff\x80\r\nservice data";
  for ending in ["\n", "\r\n"] {
    let bytes = [nonce.as_bytes(), ending.as_bytes(), payload].concat();
    let mut reader = BufReader::new(bytes.as_slice());
    read_nonce(&mut reader, &nonce).await.unwrap();
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).await.unwrap();
    assert_eq!(remaining, payload);
  }
  for bytes in [
    b"debug1: Entering interactive session.\n".as_slice(),
    b"wrong nonce\n",
    &[b'x'; MAX_READY_BYTES + 1],
    b"",
  ] {
    assert!(
      read_nonce(&mut BufReader::new(bytes), &nonce)
        .await
        .is_err()
    );
  }
}

#[tokio::test]
async fn readiness_accepts_fragmented_output_without_consuming_the_following_stream() {
  let nonce = readiness_nonce();
  let (mut writer, reader) = tokio::io::duplex(8);
  let message = nonce.clone();
  let sender = tokio::spawn(async move {
    for byte in message.bytes().chain(b"\r\n\0binary".iter().copied()) {
      writer.write_all(&[byte]).await.unwrap();
    }
  });
  let mut reader = BufReader::new(reader);
  read_nonce(&mut reader, &nonce).await.unwrap();
  let mut payload = Vec::new();
  reader.read_to_end(&mut payload).await.unwrap();
  sender.await.unwrap();
  assert_eq!(payload, b"\0binary");
}

fn fixture(nonce: Option<&str>, short: bool) -> Command {
  #[cfg(unix)]
  let mut command = {
    let mut command = Command::new("sh");
    command
      .arg("-c")
      .arg(if nonce.is_some() {
        "printf '%s\\n' \"$1\"; exec sleep \"$2\""
      } else {
        "exec sleep \"$2\""
      })
      .arg("fixture")
      .arg(nonce.unwrap_or(""))
      .arg(if short { "0.15" } else { "60" });
    command
  };
  #[cfg(windows)]
  let mut command = {
    let mut command = Command::new("cmd.exe");
    let prefix = nonce.map_or(String::new(), |nonce| format!("echo {nonce}& "));
    command.args(["/D", "/C"]).arg(format!(
      "{prefix}ping -n {} 127.0.0.1 >nul",
      if short { 2 } else { 60 }
    ));
    command
  };
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  command
}

async fn bound_forward() -> (TcpListener, LocalPortForward) {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = forward(listener.local_addr().unwrap().port());
  (listener, forward)
}

#[tokio::test]
async fn occupied_ports_and_explicit_master_reuse_fail_before_native_ssh_starts() {
  let external = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let forward = forward(external.local_addr().unwrap().port());
  let mut registry = Registry::default();
  let error = registry.start(&target(), &forward).await.unwrap_err();
  assert!(error.to_string().contains("already in use"));
  let mut target = target();
  target.use_ssh_config_master = Some(true);
  let error = registry.start(&target, &forward).await.unwrap_err();
  assert_eq!(error.code(), "ctld_platform_unsupported");
  assert!(error.to_string().contains("ControlMaster"));
  assert!(registry.listeners.is_empty());
  assert!(registry.check(&target, &forward).is_err());
}

#[tokio::test]
async fn cancellation_during_authentication_releases_reserved_listener() {
  let (listener, forward) = bound_forward().await;
  let mut registry = Registry::default();
  let nonce = readiness_nonce();
  let target = target();
  let start = registry.start_listener(&target, &forward, fixture(None, false), nonce, listener);
  assert!(
    tokio::time::timeout(Duration::from_millis(100), start)
      .await
      .is_err()
  );
  assert!(registry.listeners.is_empty());
  TcpListener::bind((forward.bind_address.as_str(), forward.local_port))
    .await
    .unwrap();
}

#[tokio::test]
async fn authenticated_anchor_exit_changes_passive_status_and_releases_its_listener() {
  let (listener, forward) = bound_forward().await;
  let mut registry = Registry::default();
  let nonce = readiness_nonce();
  registry
    .start_listener(
      &target(),
      &forward,
      fixture(Some(&nonce), true),
      nonce,
      listener,
    )
    .await
    .unwrap();
  registry.check(&target(), &forward).unwrap();
  tokio::time::timeout(Duration::from_secs(5), async {
    while registry.check(&target(), &forward).is_ok() {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
  })
  .await
  .unwrap();
  registry.cancel(&target(), &forward).await;
  assert!(registry.listeners.is_empty());
  TcpListener::bind((forward.bind_address.as_str(), forward.local_port))
    .await
    .unwrap();
}

#[tokio::test]
async fn disabling_or_dropping_an_owned_native_listener_releases_the_port() {
  for cancel in [false, true] {
    let (listener, forward) = bound_forward().await;
    let mut registry = Registry::default();
    let nonce = readiness_nonce();
    registry
      .start_listener(
        &target(),
        &forward,
        fixture(Some(&nonce), false),
        nonce,
        listener,
      )
      .await
      .unwrap();
    registry.check(&target(), &forward).unwrap();
    if cancel {
      registry.cancel(&target(), &forward).await;
    }
    drop(registry);
    tokio::time::timeout(Duration::from_secs(2), async {
      loop {
        if TcpListener::bind((forward.bind_address.as_str(), forward.local_port))
          .await
          .is_ok()
        {
          break;
        }
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
  }
}

#[tokio::test]
async fn an_exited_channel_drains_its_final_response_even_when_ssh_stdin_is_closed() {
  let nonce = readiness_nonce();
  #[cfg(unix)]
  let (mut command, expected) = {
    let mut command = Command::new("sh");
    command.args([
      "-c",
      "printf '%s\\n' \"$1\"; printf '%s' \"$2\"",
      "fixture",
      &nonce,
      "final-response",
    ]);
    (command, b"final-response".as_slice())
  };
  #[cfg(windows)]
  let (mut command, expected) = {
    let mut command = Command::new("cmd.exe");
    command
      .args(["/D", "/C"])
      .arg(format!("echo {nonce}& echo final-response"));
    (command, b"final-response\r\n".as_slice())
  };
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut process = Process::spawn(command).unwrap();
  // Child::wait closes its stdin automatically. Observe the already-exited
  // root without consuming our pipe so the relay exercises a real BrokenPipe.
  loop {
    if let Some(status) = process.try_wait().unwrap() {
      assert!(status.success());
      break;
    }
    tokio::task::yield_now().await;
  }
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let mut client = TcpStream::connect(listener.local_addr().unwrap())
    .await
    .unwrap();
  let (stream, _) = listener.accept().await.unwrap();
  let relay = tokio::spawn(async move { relay_process(stream, process, &nonce).await });
  client
    .write_all(b"request hitting closed SSH stdin")
    .await
    .unwrap();
  client.shutdown().await.unwrap();
  let mut response = Vec::new();
  tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut response))
    .await
    .unwrap()
    .unwrap();
  assert_eq!(response, expected);
  assert!(matches!(
    relay.await.unwrap().unwrap(),
    ChannelOutcome::Completed
  ));
}

#[tokio::test]
async fn a_local_tcp_reset_cancels_its_channel_without_reporting_an_ssh_failure() {
  let nonce = readiness_nonce();
  let process = Process::spawn(fixture(Some(&nonce), false)).unwrap();
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let client = TcpStream::connect(listener.local_addr().unwrap())
    .await
    .unwrap();
  let (stream, _) = listener.accept().await.unwrap();
  client.set_zero_linger().unwrap();
  drop(client);
  let outcome = tokio::time::timeout(
    Duration::from_secs(5),
    relay_process(stream, process, &nonce),
  )
  .await
  .unwrap()
  .unwrap();
  assert!(matches!(outcome, ChannelOutcome::ClientClosed));
}

#[cfg(windows)]
#[tokio::test]
async fn dropping_the_owned_job_kills_an_already_running_descendant_listener() {
  use base64::Engine as _;

  fn encoded(script: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(
      script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>(),
    )
  }

  let nonce = readiness_nonce();
  let child = encoded(
    "$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0); $listener.Start(); Write-Output $listener.LocalEndpoint.Port; Start-Sleep -Seconds 60",
  );
  let parent = encoded(&format!(
    "Write-Output '{nonce}'; $child = Start-Process -FilePath \"$PSHOME\\powershell.exe\" -NoNewWindow -PassThru -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{child}'; Start-Sleep -Seconds 60"
  ));
  let mut command = Command::new("powershell.exe");
  command
    .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &parent])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut process = Process::spawn(command).unwrap();
  let mut stdout = BufReader::new(process.stdout().unwrap());
  wait_anchor_ready(&mut process, &mut stdout, &nonce)
    .await
    .unwrap();
  let mut port = String::new();
  tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut port))
    .await
    .unwrap()
    .unwrap();
  let port: u16 = port.trim().parse().unwrap();
  tokio::time::timeout(Duration::from_secs(10), async {
    loop {
      if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
        break;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .unwrap();
  drop(process);
  tokio::time::timeout(Duration::from_secs(5), async {
    loop {
      if TcpListener::bind(("127.0.0.1", port)).await.is_ok() {
        break;
      }
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  })
  .await
  .unwrap();
}
