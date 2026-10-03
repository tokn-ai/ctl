use super::*;

#[tokio::test]
async fn preface_discards_startup_noise_and_preserves_identity_and_stream_bytes() {
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
async fn successful_setup_keeps_buffered_tcp_bytes_and_half_close_reaps_the_ssh_process() {
  use tokio::io::AsyncWriteExt as _;
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
