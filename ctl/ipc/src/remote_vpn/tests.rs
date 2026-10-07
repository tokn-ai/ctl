use super::*;

#[test]
fn recovery_retries_transport_interruptions_and_known_timeouts_only() {
  for error in [
    Error::Io(io::ErrorKind::UnexpectedEof.into()),
    Error::Codec(crate::CodecError::Io(io::ErrorKind::ConnectionReset.into())),
    Error::Timeout,
    Error::MasterUnavailable,
    Error::ConnectionClosed("protocol offer"),
    Error::ConnectionClosed("request response"),
    Error::Remote {
      code: "request_timeout".into(),
      message: "Remote VPN request timed out".into(),
    },
    Error::Remote {
      code: "vpn_connection_timeout".into(),
      message: "Remote VPN connection timed out".into(),
    },
  ] {
    assert!(error.is_retryable_connection(), "{error}");
  }
  for error in [
    Error::Io(io::ErrorKind::InvalidData.into()),
    Error::Io(io::ErrorKind::PermissionDenied.into()),
    Error::Io(io::ErrorKind::NotFound.into()),
    Error::Codec(crate::CodecError::FrameTooLarge {
      actual: usize::MAX,
      maximum: crate::MAX_FRAME_SIZE,
    }),
    Error::Codec(crate::CodecError::Json(
      serde_json::from_str::<Request>("invalid").err().unwrap(),
    )),
    Error::UnsupportedAgent,
    Error::UnsupportedProtocol,
    Error::IdentityMismatch,
    Error::InvalidRequest("invalid destination".into()),
    Error::UnexpectedResponse,
    Error::SshFailed,
    Error::Remote {
      code: "invalid_request".into(),
      message: "Invalid remote VPN request".into(),
    },
  ] {
    assert!(!error.is_retryable_connection(), "{error}");
  }
}

#[tokio::test]
async fn a_closed_negotiation_channel_is_not_a_contract_mismatch() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let error = negotiate_contract(&mut &[][..], &mut Vec::new())
    .await
    .unwrap_err();
  assert!(matches!(error, Error::ConnectionClosed("protocol offer")));
  assert!(error.is_retryable_connection());
  let error = accept_contract(&mut &[][..], &mut Vec::new())
    .await
    .unwrap_err();
  assert!(matches!(
    error,
    Error::ConnectionClosed("protocol selection")
  ));
  assert!(error.is_retryable_connection());
}

#[cfg(unix)]
#[tokio::test]
async fn negotiation_peer_child() {
  use tokio::io::AsyncWriteExt as _;
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let Some(mode) = std::env::var_os("CTL_REMOTE_VPN_NEGOTIATION_TEST") else {
    return;
  };
  if mode == "noisy_startup" {
    noisy_diagnostics();
    std::process::exit(255);
  }
  if mode == "hold_diagnostics" {
    std::future::pending::<()>().await;
  }
  let (mut reader, mut writer) = crate::stdio::take().unwrap();
  writer.write_all(PREFACE).await.unwrap();
  accept_contract(&mut reader, &mut writer).await.unwrap();
  if mode == "close_identity" {
    return;
  }
  let identity = ctl_proto::RemoteIdentity {
    remote_id: "a060a4f4-2225-4d3c-8c8b-c9c8c2b3bc69".into(),
    agent_version: "0.1.0".into(),
    build: None,
    ctmux_restart_supported: false,
    bundle: None,
    protocols: ctl_proto::agent_protocols(),
  };
  ctl_proto::write_identity(&mut writer, &identity)
    .await
    .unwrap();
  assert!(matches!(
    crate::read_frame::<_, Request>(&mut reader).await.unwrap(),
    Some(Request::List)
  ));
  if mode == "noisy_live" {
    noisy_diagnostics();
    crate::write_frame(&mut writer, &Response::Connected)
      .await
      .unwrap();
  }
}

#[cfg(unix)]
fn noisy_diagnostics() {
  use std::io::Write as _;
  let mut stderr = std::io::stderr().lock();
  stderr.write_all(b"\x1b[2JVPN_SSH_DIAGNOSTIC\n").unwrap();
  // Larger than a pipe's capacity: retaining only a prefix must still drain
  // the whole stream, during startup and after a live channel is returned.
  for _ in 0..256 {
    stderr.write_all(&[b'x'; 4096]).unwrap();
  }
}

#[cfg(unix)]
fn negotiation_command(mode: &str) -> Command {
  let mut command = Command::new(std::env::current_exe().unwrap());
  command
    .args([
      "--exact",
      "remote_vpn::tests::negotiation_peer_child",
      "--nocapture",
    ])
    .env("CTL_REMOTE_VPN_NEGOTIATION_TEST", mode);
  command
}

#[cfg(unix)]
fn fixture_target() -> SshTarget {
  SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  }
}

#[cfg(unix)]
#[tokio::test]
async fn headless_startup_captures_bounded_diagnostics_and_preserves_recovery_policy() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for pinned in [false, true] {
    let mut client = Client::new(fixture_target(), None).with_terminal_interaction(false);
    if pinned {
      client = client.with_control_path("/does/not/exist/vpn-test-master".into());
    }
    let error = tokio::time::timeout(
      Duration::from_secs(5),
      client.open_command(negotiation_command("noisy_startup")),
    )
    .await
    .expect("noisy SSH stderr must not fill its pipe and block startup")
    .err()
    .expect("the fixture SSH process exits unsuccessfully");
    assert_eq!(error.is_retryable_connection(), pinned);
    let Error::SshDiagnostics {
      source,
      diagnostics,
    } = error
    else {
      panic!("SSH stderr must be returned through the caller's error");
    };
    assert!(diagnostics.contains("VPN_SSH_DIAGNOSTIC"));
    assert!(diagnostics.len() <= MAX_DIAGNOSTICS);
    assert!(!diagnostics.contains('\x1b'));
    if pinned {
      assert!(matches!(*source, Error::MasterUnavailable));
    } else {
      assert!(matches!(*source, Error::SshFailed));
    }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn headless_live_channels_continue_draining_after_the_diagnostic_limit() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let client = Client::new(fixture_target(), None).with_terminal_interaction(false);
  tokio::time::timeout(Duration::from_secs(5), async {
    let mut stream = client
      .open_command(negotiation_command("noisy_live"))
      .await
      .unwrap();
    assert!(matches!(
      send_request(&mut stream, &Request::List, Duration::from_secs(3))
        .await
        .unwrap(),
      Response::Connected
    ));
    let retained = Arc::clone(&stream.diagnostics.as_ref().unwrap().bytes);
    stream.finish().await.unwrap();
    assert_eq!(retained.lock().unwrap().len(), MAX_DIAGNOSTICS);
  })
  .await
  .expect("live SSH stderr must stay drained after the captured prefix is full");
}

#[cfg(unix)]
#[tokio::test]
async fn headless_clients_disable_ssh_terminal_authentication_without_a_master() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let command = Client::new(fixture_target(), None)
    .with_terminal_interaction(false)
    .command()
    .await
    .unwrap();
  assert!(
    command
      .as_std()
      .get_args()
      .any(|argument| argument == "BatchMode=yes")
  );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_diagnostic_collection_keeps_the_drain_owned_until_drop() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut command = negotiation_command("hold_diagnostics");
  command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  let mut child = command.spawn().unwrap();
  let mut diagnostics = Diagnostics::start(child.stderr.take().unwrap());
  let drain = diagnostics.task.as_ref().unwrap().abort_handle();
  assert!(
    tokio::time::timeout(Duration::from_millis(50), diagnostics.finish())
      .await
      .is_err(),
    "the fixture keeps stderr open while collection is cancelled"
  );
  drop(diagnostics);
  tokio::time::timeout(Duration::from_secs(1), async {
    while !drain.is_finished() {
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("dropping cancelled diagnostics must abort its pending drain task");
  child.kill().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn channel_loss_after_selection_or_request_remains_recoverable() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let client = Client::new(target, None);
  for mode in ["close_identity", "close_response"] {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
      .args([
        "--exact",
        "remote_vpn::tests::negotiation_peer_child",
        "--nocapture",
      ])
      .env("CTL_REMOTE_VPN_NEGOTIATION_TEST", mode);
    let error = tokio::time::timeout(Duration::from_secs(5), async {
      match client.open_command(command).await {
        Err(error) => error,
        Ok(mut stream) => send_request(&mut stream, &Request::List, Duration::from_secs(3))
          .await
          .unwrap_err(),
      }
    })
    .await
    .expect("the closed channel must be detected promptly");
    if mode == "close_identity" {
      assert!(matches!(error, Error::Io(ref io) if io.kind() == io::ErrorKind::UnexpectedEof));
    } else {
      assert!(matches!(error, Error::ConnectionClosed("request response")));
    }
    assert!(error.is_retryable_connection(), "{mode}: {error}");
  }
}

#[tokio::test]
async fn preface_discards_startup_noise_and_preserves_identity_and_stream_bytes() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let identity = ctl_proto::RemoteIdentity {
    remote_id: "a060a4f4-2225-4d3c-8c8b-c9c8c2b3bc69".into(),
    agent_version: "0.1.0".into(),
    build: None,
    ctmux_restart_supported: false,
    bundle: None,
    protocols: ctl_proto::agent_protocols(),
  };
  let mut bytes = b"startup without a newline".to_vec();
  bytes.extend_from_slice(PREFACE);
  crate::write_frame(&mut bytes, &protocol_offer())
    .await
    .unwrap();
  ctl_proto::write_identity(&mut bytes, &identity)
    .await
    .unwrap();
  bytes.extend_from_slice(b"\0\xfftcp");
  let mut reader = bytes.as_slice();
  read_preface(&mut reader).await.unwrap();
  let mut selection = Vec::new();
  assert_eq!(
    negotiate_contract(&mut reader, &mut selection)
      .await
      .unwrap(),
    CONTRACT_V1_0_1
  );
  assert_eq!(
    ctl_proto::read_identity(&mut reader).await.unwrap(),
    identity
  );
  assert_eq!(reader, b"\0\xfftcp");
}

#[tokio::test]
async fn bounded_frames_reject_unknown_operations_and_oversized_requests() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut bytes = Vec::new();
  crate::write_frame(&mut bytes, &serde_json::json!({ "type": "ensure_master" }))
    .await
    .unwrap();
  assert!(
    crate::read_frame::<_, Request>(&mut bytes.as_slice())
      .await
      .is_err()
  );
  assert!(
    crate::read_frame::<_, Request>(&mut u32::MAX.to_be_bytes().as_slice())
      .await
      .is_err()
  );
  bytes.clear();
  crate::write_frame(
    &mut bytes,
    &Request::Connect {
      connection_id: "work".into(),
      host: "internal.example".into(),
      port: 22,
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    crate::read_frame::<_, Request>(&mut bytes.as_slice())
      .await
      .unwrap(),
    Some(Request::Connect { port: 22, .. })
  ));
}

#[tokio::test]
async fn old_or_missing_agent_prefaces_produce_an_update_instruction() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  for output in [
    b"".as_slice(),
    b"ctl-ssh-nf\n",
    b"ctl-vpn-v1\n",
    b"ctl-vpn-v2\n",
    b"unrecognized subcommand vpn\n",
  ] {
    assert!(matches!(
      read_preface(&mut &output[..]).await,
      Err(Error::UnsupportedAgent)
    ));
  }
}

#[tokio::test]
async fn negotiation_selects_only_explicit_published_contracts_before_any_vpn_input() {
  use ctl_core::protocol::{ProtocolOffer, ProtocolVersion};
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let newer = ProtocolVersion::new(1, 1, 3);
  let mut bytes = Vec::new();
  crate::write_frame(
    &mut bytes,
    &ProtocolOffer::new(3, newer, &[CONTRACT_V1_0_1, newer]),
  )
  .await
  .unwrap();
  bytes.extend_from_slice(b"identity then TCP");
  let mut remaining = bytes.as_slice();
  let mut output = Vec::new();
  assert_eq!(
    negotiate_contract(&mut remaining, &mut output)
      .await
      .unwrap(),
    CONTRACT_V1_0_1
  );
  assert_eq!(remaining, b"identity then TCP");
  let mut output = output.as_slice();
  let selected: ProtocolSelection = crate::read_frame(&mut output).await.unwrap().unwrap();
  assert_eq!(selected.protocol_version, CONTRACT_V1_0_1);
  assert_eq!(output, [] as [u8; 0]);

  for unsupported in [newer, ProtocolVersion::new(2, 0, 3)] {
    let mut bytes = Vec::new();
    crate::write_frame(
      &mut bytes,
      &ProtocolOffer::new(3, unsupported, &[unsupported]),
    )
    .await
    .unwrap();
    let mut output = Vec::new();
    assert!(matches!(
      negotiate_contract(&mut bytes.as_slice(), &mut output).await,
      Err(Error::UnsupportedProtocol)
    ));
    assert_eq!(output, [] as [u8; 0]);
  }
}

#[tokio::test]
async fn malformed_offers_and_unpublished_selections_are_rejected_before_identity() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let mut malformed = Vec::new();
  crate::write_frame(
    &mut malformed,
    &serde_json::json!({
      "build": 1, "version": "1.0.1", "supported_versions": ["1.0.1", "1.0.1"]
    }),
  )
  .await
  .unwrap();
  let mut output = Vec::new();
  assert!(
    negotiate_contract(&mut malformed.as_slice(), &mut output)
      .await
      .is_err()
  );
  assert_eq!(output, [] as [u8; 0]);

  for version in ["1.1.3", "2.0.3"] {
    let mut bytes = Vec::new();
    crate::write_frame(
      &mut bytes,
      &serde_json::json!({ "protocol_version": version }),
    )
    .await
    .unwrap();
    let mut output = Vec::new();
    assert!(matches!(
      accept_contract(&mut bytes.as_slice(), &mut output).await,
      Err(Error::UnsupportedProtocol)
    ));
    let mut output = output.as_slice();
    let offer: ctl_core::protocol::ProtocolOffer =
      crate::read_frame(&mut output).await.unwrap().unwrap();
    assert_eq!(offer, protocol_offer());
    assert_eq!(output, [] as [u8; 0]);
  }
}

#[test]
fn remote_requests_do_not_accept_arbitrary_paths_or_invalid_tcp_destinations() {
  for (connection_id, host, port) in [
    ("", "internal", 22),
    ("work", "", 22),
    ("work", "internal\ncommand", 22),
    ("work", "internal", 0),
  ] {
    assert!(
      Request::Connect {
        connection_id: connection_id.into(),
        host: host.into(),
        port
      }
      .validate()
      .is_err()
    );
  }
  assert!(
    serde_json::from_value::<Request>(
      serde_json::json!({ "type": "start", "env_file": "/tmp/secret" })
    )
    .is_err()
  );
}

#[tokio::test]
async fn pinned_master_is_batch_and_cannot_fall_back_to_network() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let command = Client::new(target, None)
    .with_control_path("/tmp/pinned-master".into())
    .command()
    .await
    .unwrap();
  let arguments: Vec<_> = command
    .as_std()
    .get_args()
    .map(|value| value.to_str().unwrap())
    .collect();
  assert!(arguments.contains(&"BatchMode=yes"));
  assert!(arguments.contains(&"ProxyCommand=false"));
  assert!(arguments.contains(&"ControlMaster=no"));
  assert!(arguments.contains(&"/tmp/pinned-master"));
  assert_eq!(arguments.last(), Some(&REMOTE_COMMAND));
}

#[cfg(unix)]
#[tokio::test]
async fn identity_mismatch_closes_the_child_before_a_profile_can_be_sent() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let directory = std::env::temp_dir().join(format!("remote-vpn-identity-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let payload = directory.join("payload");
  let input = directory.join("input");
  let identity = ctl_proto::RemoteIdentity {
    remote_id: "a060a4f4-2225-4d3c-8c8b-c9c8c2b3bc69".into(),
    agent_version: "0.1.0".into(),
    build: None,
    ctmux_restart_supported: false,
    bundle: None,
    protocols: ctl_proto::agent_protocols(),
  };
  let mut bytes = PREFACE.to_vec();
  crate::write_frame(&mut bytes, &protocol_offer())
    .await
    .unwrap();
  ctl_proto::write_identity(&mut bytes, &identity)
    .await
    .unwrap();
  std::fs::write(&payload, bytes).unwrap();
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let client = Client::new(target, Some("10b36b22-cebb-4d03-ad65-a4f76b5ef410".into()));
  let mut command = Command::new("sh");
  command
    .arg("-c")
    .arg("cat \"$1\"; exec cat > \"$2\"")
    .arg("remote-vpn-test")
    .arg(&payload)
    .arg(&input);
  assert!(matches!(
    client.open_command(command).await,
    Err(Error::IdentityMismatch)
  ));
  // Credentials can only be sent after open returns a verified stream.
  let bytes = std::fs::read(&input).unwrap_or_default();
  if !bytes.is_empty() {
    let mut remaining = bytes.as_slice();
    let selected: ProtocolSelection = crate::read_frame(&mut remaining).await.unwrap().unwrap();
    assert_eq!(selected.protocol_version, CONTRACT_V1_0_1);
    assert_eq!(remaining, [] as [u8; 0]);
  }
  std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn failed_ssh_startup_is_not_reported_as_missing_vpn_support() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let client = Client::new(target, None);
  for (exit_code, ssh_failure) in [(255, true), (2, false)] {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!("exit {exit_code}"));
    let error = client.open_command(command).await.err().unwrap();
    if ssh_failure {
      assert!(matches!(error, Error::SshFailed));
    } else {
      assert!(matches!(error, Error::UnsupportedAgent));
    }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn a_disappeared_pinned_master_can_be_reprepared_after_failed_startup() {
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let directory = std::env::temp_dir().join(format!(
    "remote-vpn-master-{}-{}",
    std::process::id(),
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_nanos()
  ));
  std::fs::create_dir(&directory).unwrap();
  let path = directory.join("control");
  let client = Client::new(target, None).with_control_path(path.clone());
  for present in [false, true] {
    if present {
      std::fs::write(&path, b"existing control endpoint").unwrap();
    }
    let mut command = Command::new("sh");
    command.args(["-c", "exit 255"]);
    let error = client.open_command(command).await.err().unwrap();
    assert_eq!(error.is_retryable_connection(), !present);
    if present {
      assert!(matches!(error, Error::SshFailed));
    } else {
      assert!(matches!(error, Error::MasterUnavailable));
    }
  }
  std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn successful_setup_keeps_buffered_tcp_bytes_and_half_close_reaps_the_ssh_process() {
  use tokio::io::AsyncWriteExt as _;
  let _process_guard = ctl_core::test_fixtures::ProcessGuard::acquire().await;
  let directory = std::env::temp_dir().join(format!("remote-vpn-stream-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let payload = directory.join("payload");
  let identity = ctl_proto::RemoteIdentity {
    remote_id: "a060a4f4-2225-4d3c-8c8b-c9c8c2b3bc69".into(),
    agent_version: "0.1.0".into(),
    build: None,
    ctmux_restart_supported: false,
    bundle: None,
    protocols: ctl_proto::agent_protocols(),
  };
  let mut bytes = PREFACE.to_vec();
  crate::write_frame(&mut bytes, &protocol_offer())
    .await
    .unwrap();
  ctl_proto::write_identity(&mut bytes, &identity)
    .await
    .unwrap();
  crate::write_frame(&mut bytes, &Response::Connected)
    .await
    .unwrap();
  bytes.extend_from_slice(b"\0\xfftcp");
  std::fs::write(&payload, bytes).unwrap();
  let target = SshTarget {
    destination: "vpn-owner".into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: vec![],
  };
  let client = Client::new(target, Some(identity.remote_id.clone()));
  let mut command = Command::new("sh");
  command
    .arg("-c")
    .arg("cat \"$1\"; exec cat > /dev/null")
    .arg("remote-vpn-test")
    .arg(payload);
  let mut stream = client.open_command(command).await.unwrap();
  assert_eq!(stream.identity(), &identity);
  assert_eq!(stream.protocol_version(), CONTRACT_V1_0_1);
  assert!(matches!(
    send_request(
      &mut stream,
      &Request::Connect {
        connection_id: "work".into(),
        host: "internal.example".into(),
        port: 22
      },
      Duration::from_secs(3)
    )
    .await
    .unwrap(),
    Response::Connected
  ));
  let mut tcp = [0; 5];
  stream.read_exact(&mut tcp).await.unwrap();
  assert_eq!(&tcp, b"\0\xfftcp");
  stream.write_all(b"request").await.unwrap();
  let waiter = stream.waiter.take().unwrap();
  stream.shutdown().await.unwrap();
  assert!(
    tokio::time::timeout(Duration::from_secs(3), waiter)
      .await
      .expect("half-close must deliver EOF to the SSH child")
      .unwrap()
      .unwrap()
      .success()
  );
  drop(stream);
  std::fs::remove_dir_all(directory).unwrap();
}
