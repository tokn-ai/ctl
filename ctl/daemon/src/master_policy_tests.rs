use std::future::{Future, ready};

use super::*;

fn target(alias: Option<&str>) -> SshTarget {
  SshTarget {
    ssh_config_alias: alias.map(str::to_owned),
    use_ssh_config_master: None,
    destination: "builder".into(),
    hostname: Some("example.test".into()),
    user: Some("developer".into()),
    port: Some(2222),
    identity_file: None,
    gateways: vec![],
  }
}

#[test]
fn only_configured_methods_preserve_alias_matching_and_inherit_master_options() {
  let configured = target(Some("builder"));
  let endpoint = MasterEndpoint {
    control_path: "/tmp/user-master".into(),
    shared: true,
    startup: SharedMasterStartup::Create,
  };
  let command = master_command(&configured, &endpoint);
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|item| item.to_string_lossy())
    .collect();
  for overridden in [
    "-M",
    "-S",
    "-f",
    "-N",
    "ControlMaster=yes",
    "ControlPersist=300",
  ] {
    assert!(
      !args.iter().any(|arg| arg == overridden),
      "unexpected override {overridden}"
    );
  }
  assert!(args.iter().any(|arg| arg == "HostName=example.test"));
  assert_eq!(
    &args[args.len() - 3..],
    ["--", "builder", ssh_config_master::SHARED_SESSION_COMMAND]
  );

  let managed = target(None);
  let command = master_command(&managed, &MasterEndpoint::managed(&managed));
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|item| item.to_string_lossy())
    .collect();
  assert!(args.iter().any(|arg| arg == "ControlMaster=yes"));
  assert!(args.iter().any(|arg| arg == "ControlPersist=300"));
  assert_eq!(args.last().unwrap(), "example.test");
}

#[test]
fn rejects_inconsistent_alias_metadata() {
  assert!(validate_target(&target(Some("builder"))).is_ok());
  for alias in ["different", "-option", "*", "a b"] {
    assert!(validate_target(&target(Some(alias))).is_err());
  }
}

#[tokio::test]
async fn managed_methods_do_not_evaluate_ssh_configuration() {
  let managed = target(None);
  let resolved = ssh_config_master::resolve(&managed).await.unwrap();
  assert!(!resolved.shared);
  assert_eq!(resolved.control_path, control_path(&managed));
}

#[tokio::test]
async fn opting_out_uses_a_private_master_and_keeps_alias_matching() {
  let mut configured = target(Some("builder"));
  configured.use_ssh_config_master = Some(false);
  let endpoint = ssh_config_master::resolve(&configured).await.unwrap();
  assert!(!endpoint.shared);
  let command = master_command(&configured, &endpoint);
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|item| item.to_string_lossy())
    .collect();
  assert!(args.iter().any(|arg| arg == "ControlMaster=yes"));
  assert!(args.iter().any(|arg| arg == "HostName=example.test"));
  assert_eq!(&args[args.len() - 2..], ["--", "builder"]);
  assert_ne!(
    control_path(&configured),
    control_path(&target(Some("builder")))
  );
}

#[cfg(unix)]
#[tokio::test]
async fn a_direct_method_can_adopt_and_release_a_configured_master() {
  let mut direct = target(None);
  direct.use_ssh_config_master = Some(true);
  let endpoint = MasterEndpoint {
    control_path: "/tmp/external-direct-master".into(),
    shared: true,
    startup: SharedMasterStartup::Create,
  };
  let state = State::default();
  assert!(state.endpoint(&direct).is_none());
  state.adopt(&direct, &endpoint, None);
  assert_eq!(
    state.endpoint(&direct).unwrap().control_path,
    endpoint.control_path
  );
  let command = master_command(&direct, &endpoint);
  let args: Vec<_> = command.as_std().get_args().collect();
  assert_eq!(args[args.len() - 2], "example.test");
  let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
  disconnect_master(&mut server, &state, &direct)
    .await
    .unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::MasterDisconnected)
  ));
  assert!(state.endpoint(&direct).is_none());
  assert!(state.target(&direct).is_paused());
}

#[cfg(unix)]
#[tokio::test]
async fn an_explicit_default_and_omitted_preference_share_disconnect_state() {
  for alias in [None, Some("builder")] {
    let state = Arc::new(State::default());
    let mut selected = target(alias);
    selected.use_ssh_config_master = Some(alias.is_some());
    for request in [
      ClientMessage::DisconnectMaster { target: selected },
      ClientMessage::ConnectionStatus {
        target: target(alias),
      },
    ] {
      let (mut client, server) = ctld_ipc::Stream::pair().unwrap();
      let server = tokio::spawn(handle_connection(server, Arc::clone(&state)));
      handshake(&mut client).await.unwrap();
      ctld_ipc::write_frame(&mut client, &request).await.unwrap();
      let response = ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
        .await
        .unwrap();
      assert!(matches!(
        response,
        Some(
          ServerMessage::MasterDisconnected
            | ServerMessage::ConnectionStatus {
              connected: false,
              manually_disconnected: true
            }
        )
      ));
      server.await.unwrap().unwrap();
    }
  }
}

#[cfg(unix)]
#[tokio::test]
async fn shared_disconnect_closes_only_our_anchor_and_preserves_external_socket() {
  let path = std::env::temp_dir().join(format!("ctld-external-{}", uuid::Uuid::new_v4()));
  // A regular file deliberately fails ssh -O exit: successful disconnect proves
  // it neither sends that command nor unlinks the user's configured path.
  std::fs::write(&path, "external owner").unwrap();
  let mut anchor = Command::new("cat")
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .spawn()
    .unwrap();
  let state = State::default();
  let configured = target(Some("builder"));
  let endpoint = MasterEndpoint {
    control_path: path.clone(),
    shared: true,
    startup: SharedMasterStartup::Create,
  };
  state.adopt(&configured, &endpoint, anchor.stdin.take());
  state.adopt(&configured, &endpoint, None);
  assert!(
    state.configured_connections.lock().unwrap()[&target_key(&configured)]
      .anchor
      .is_some()
  );
  let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
  disconnect_master(&mut server, &state, &configured)
    .await
    .unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::MasterDisconnected)
  ));
  assert!(state.endpoint(&configured).is_none());
  assert!(state.target(&configured).is_paused());
  assert_eq!(std::fs::read_to_string(&path).unwrap(), "external owner");
  assert!(
    tokio::time::timeout(Duration::from_secs(2), anchor.wait())
      .await
      .unwrap()
      .unwrap()
      .success()
  );
  connection_status(&mut server, &state, &configured)
    .await
    .unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::ConnectionStatus {
      connected: false,
      manually_disconnected: true
    })
  ));
  assert!(matches!(
    master_status(&mut server, &state, &configured).await,
    Err(RequestError::HostDisconnected)
  ));
  std::fs::remove_file(path).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn an_external_socket_is_not_a_connection_until_adopted() {
  let state = State::default();
  let configured = target(Some("builder"));
  let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
  assert!(state.endpoint(&configured).is_none());
  master_status(&mut server, &state, &configured)
    .await
    .unwrap();
  assert!(matches!(
    ctld_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::AuthenticationRequired)
  ));
}

#[cfg(unix)]
#[tokio::test]
async fn a_stale_private_fallback_socket_does_not_block_reconnection_with_forwards() {
  struct ReadyControl;
  impl port_forwarding::ForwardControl for ReadyControl {
    fn is_ready(&self, _: &SshTarget) -> impl Future<Output = bool> + Send {
      ready(true)
    }
    fn change(
      &self,
      _: &SshTarget,
      _: &LocalPortForward,
      _: bool,
    ) -> impl Future<Output = Result<(), RequestError>> + Send {
      ready(Ok(()))
    }
  }
  let root = std::env::temp_dir().join(format!("ctld-stale-{}", uuid::Uuid::new_v4()));
  let directory = root.join("masters");
  prepare_private_directory(&root).unwrap();
  prepare_private_directory(&directory).unwrap();
  let endpoint = MasterEndpoint {
    control_path: directory.join("stale"),
    shared: false,
    startup: SharedMasterStartup::PrivateFallback,
  };
  std::fs::write(&endpoint.control_path, "stale socket").unwrap();
  let state = State::default();
  let configured = target(Some("builder"));
  state.adopt(&configured, &endpoint, None);
  state
    .forwards
    .lock()
    .await
    .configure(
      &ReadyControl,
      configured.clone(),
      LocalPortForward {
        forward_id: "web".into(),
        bind_address: "127.0.0.1".into(),
        local_port: 18080,
        remote_host: "localhost".into(),
        remote_port: 80,
      },
      true,
    )
    .await
    .unwrap();
  assert!(
    !reuse_master_or_prepare(&state, &configured, &endpoint)
      .await
      .unwrap()
  );
  assert!(!endpoint.control_path.exists());
  std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn ending_shared_attempt_never_kills_before_or_after_socket_publication() {
  for published in [false, true] {
    let path = std::env::temp_dir().join(format!("ctld-published-{}", uuid::Uuid::new_v4()));
    if published {
      std::fs::write(&path, "external master").unwrap();
    }
    let output = path.with_extension("exit");
    let child = Command::new("sh")
      .arg("-c")
      .arg("cat >/dev/null; printf 'external master' >\"$1\"; printf finished >\"$2\"")
      .arg("fixture")
      .arg(&path)
      .arg(&output)
      .stdin(Stdio::piped())
      .spawn()
      .unwrap();
    assert_eq!(path.exists(), published);
    // Closing our anchor can coincide with publishing the socket. Neither
    // ordering permits killing a process that may serve external channels.
    release_shared_process(child);
    tokio::time::timeout(Duration::from_secs(2), async {
      while std::fs::read_to_string(&output).ok().as_deref() != Some("finished") {
        sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external master");
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(output).unwrap();
  }
}

#[test]
fn disappearing_reuse_only_masters_do_not_authorize_shared_startup() {
  let configured = target(Some("builder"));
  let path = PathBuf::from("/tmp/external-master");
  let endpoint = |startup| MasterEndpoint {
    control_path: path.clone(),
    shared: true,
    startup,
  };
  let fallback = endpoint(SharedMasterStartup::PrivateFallback)
    .after_missing_master(&configured)
    .unwrap();
  assert!(!fallback.shared);
  assert_eq!(fallback.control_path, control_path(&configured));
  assert!(matches!(
    endpoint(SharedMasterStartup::ExternalOnly).after_missing_master(&configured),
    Err(RequestError::SshConfig(message)) if message.contains("started outside rmux")
  ));
  let creatable = endpoint(SharedMasterStartup::Create)
    .after_missing_master(&configured)
    .unwrap();
  assert!(creatable.shared);
  assert_eq!(creatable.control_path, path);

  // Fail closed if a caller bypasses the missing-master policy transition.
  for startup in [
    SharedMasterStartup::PrivateFallback,
    SharedMasterStartup::ExternalOnly,
  ] {
    assert!(matches!(
      start_master(&configured, &endpoint(startup), "unused-token"),
      Err(RequestError::SshConfig(_))
    ));
  }
}
