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
    "ServerAliveInterval=10",
    "ServerAliveCountMax=3",
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
  assert!(args.iter().any(|arg| arg == "ServerAliveInterval=10"));
  assert!(args.iter().any(|arg| arg == "ServerAliveCountMax=3"));
  assert_eq!(args.last().unwrap(), "example.test");
}

#[cfg(unix)]
#[tokio::test]
async fn private_master_keepalives_override_disabled_user_policy_without_connecting() {
  let path = std::env::temp_dir().join(format!("ctld-keepalives-{}", uuid::Uuid::new_v4()));
  let guard = SocketGuard(path);
  std::fs::write(
    &guard.0,
    "Host *\n  ServerAliveInterval 0\n  ServerAliveCountMax 0\n",
  )
  .unwrap();
  let managed = target(None);
  let master = master_command(&managed, &MasterEndpoint::managed(&managed));
  let output = Command::new(SSH_PROGRAM)
    .args(["-G", "-F"])
    .arg(&guard.0)
    .args(master.as_std().get_args())
    .stdin(Stdio::null())
    .kill_on_drop(true)
    .output()
    .await
    .unwrap();
  assert!(output.status.success());
  let config = String::from_utf8(output.stdout).unwrap();
  assert!(config.lines().any(|line| line == "serveraliveinterval 10"));
  assert!(config.lines().any(|line| line == "serveralivecountmax 3"));
}

#[test]
fn rejects_inconsistent_alias_metadata() {
  assert!(validate_target(&target(Some("builder"))).is_ok());
  for alias in ["different", "-option", "*", "a b"] {
    assert!(validate_target(&target(Some(alias))).is_err());
  }
}

#[test]
fn gateway_hostname_overrides_keep_the_alias_in_the_proxy_route() {
  let mut routed = target(None);
  routed.gateways.push(SshGateway {
    kind: GatewayKind::Ssh,
    vpn: None,
    destination: "configured-key-alias".into(),
    hostname: Some("100.64.0.2".into()),
    user: Some("operator".into()),
    port: Some(2222),
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  });
  routed.use_ssh_config_master = Some(true);
  assert!(!routed.uses_ssh_config_master());
  let command = master_command(&routed, &MasterEndpoint::managed(&routed));
  let executable = std::env::current_exe().unwrap();
  let proxy = ctl_ipc::proxy_command_with_executable(&routed.gateways, &executable);
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|arg| arg.to_string_lossy())
    .collect();
  assert!(
    args
      .iter()
      .any(|arg| arg == &format!("ProxyCommand={proxy}"))
  );
  assert!(!args.iter().any(|arg| arg.starts_with("ProxyJump=")));
}

#[tokio::test]
async fn managed_methods_do_not_evaluate_ssh_configuration() {
  let managed = target(None);
  let resolved = ssh_config_master::resolve(&managed).await.unwrap();
  assert!(!resolved.shared);
  assert_eq!(resolved.control_path, control_path(&managed));
}

#[tokio::test]
async fn vpn_routes_force_a_private_master_and_use_stable_profile_identity() {
  let mut routed = target(Some("builder"));
  routed.use_ssh_config_master = Some(true);
  routed.gateways.push(SshGateway {
    kind: GatewayKind::Vpn,
    vpn: Some(ctl_ipc::VpnGateway {
      connection_id: "saved-vpn".into(),
      socket_path: std::env::temp_dir().join("test-vpn-owner.sock"),
      expected_remote_id: None,
    }),
    destination: "saved-vpn".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: SshGatewayMode::Automatic,
  });
  assert!(validate_target(&routed).is_ok());
  assert!(!routed.uses_ssh_config_master());
  let endpoint = ssh_config_master::resolve(&routed).await.unwrap();
  assert!(!endpoint.shared);
  let command = master_command(&routed, &endpoint);
  let args: Vec<_> = command
    .as_std()
    .get_args()
    .map(|item| item.to_string_lossy())
    .collect();
  assert!(args.iter().any(|arg| arg == "ControlMaster=yes"));
  let executable = std::env::current_exe().unwrap();
  let pinned = format!("ProxyCommand='{}' ", executable.display());
  assert!(args.iter().any(|arg| arg.starts_with(&pinned)));
  assert!(!args.iter().any(|arg| arg.starts_with("ProxyJump=")));
  let first_key = target_key(&routed);
  let first_path = control_path(&routed);
  let reconstructed: SshTarget =
    serde_json::from_slice(&serde_json::to_vec(&routed).unwrap()).unwrap();
  assert_eq!(target_key(&reconstructed), first_key);
  assert_eq!(control_path(&reconstructed), first_path);
  let mut another_owner = reconstructed.clone();
  another_owner.gateways[0].vpn.as_mut().unwrap().socket_path =
    std::env::temp_dir().join("another-owner.sock");
  assert_ne!(target_key(&another_owner), first_key);
  let mut another_profile = reconstructed;
  another_profile.gateways[0].destination = "other-vpn".into();
  another_profile.gateways[0]
    .vpn
    .as_mut()
    .unwrap()
    .connection_id = "other-vpn".into();
  assert_ne!(target_key(&another_profile), first_key);

  // A second/local reference cannot be interpreted as a remote VPN hop.
  routed.gateways.push(routed.gateways[0].clone());
  assert!(validate_target(&routed).is_err());
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
  state.adopt(&direct, &endpoint, None).unwrap();
  assert_eq!(
    state.endpoint(&direct).unwrap().control_path,
    endpoint.control_path
  );
  let command = master_command(&direct, &endpoint);
  let args: Vec<_> = command.as_std().get_args().collect();
  assert_eq!(args[args.len() - 2], "example.test");
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  disconnect_master(&mut server, &state, &direct)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
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
      let (mut client, server) = ctl_ipc::Stream::pair().unwrap();
      let server = tokio::spawn(handle_connection(server, Arc::clone(&state)));
      handshake(&mut client).await.unwrap();
      ctl_ipc::write_frame(&mut client, &request).await.unwrap();
      let response = ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
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
  state
    .adopt(&configured, &endpoint, anchor.stdin.take())
    .unwrap();
  state.adopt(&configured, &endpoint, None).unwrap();
  assert!(
    state.configured_connections.lock().unwrap()[&target_key(&configured)]
      .anchor
      .is_some()
  );
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  let correlation = state.correlation(&endpoint);
  disconnect_master(&mut server, &state, &configured)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::MasterDisconnected)
  ));
  assert!(state.endpoint(&configured).is_none());
  assert!(state.target(&configured).is_paused());
  assert_eq!(state.correlation(&endpoint), correlation);
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
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
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
  let (mut client, mut server) = ctl_ipc::Stream::pair().unwrap();
  assert!(state.endpoint(&configured).is_none());
  master_status(&mut server, &state, &configured)
    .await
    .unwrap();
  assert!(matches!(
    ctl_ipc::read_frame::<_, ServerMessage>(&mut client)
      .await
      .unwrap(),
    Some(ServerMessage::AuthenticationRequired)
  ));
}

#[cfg(unix)]
#[tokio::test]
async fn an_unknown_control_check_preserves_the_endpoint_and_correlation() {
  let path = std::env::temp_dir().join(format!("ctld-unknown-{}", uuid::Uuid::new_v4()));
  let _guard = SocketGuard(path.clone());
  // OpenSSH cannot check this endpoint. Its failure must not authorize socket
  // deletion or a new authentication attempt.
  std::fs::write(&path, "do not replace").unwrap();
  let state = State::default();
  let configured = target(Some("builder"));
  let endpoint = MasterEndpoint {
    control_path: path.clone(),
    shared: false,
    startup: SharedMasterStartup::PrivateFallback,
  };
  state.adopt(&configured, &endpoint, None).unwrap();
  let correlation = state.correlation(&endpoint);
  let error = reuse_master_or_prepare(&state, &configured, &endpoint)
    .await
    .unwrap_err();
  assert_eq!(error.code(), "ssh_status_unknown");
  assert_eq!(std::fs::read_to_string(&path).unwrap(), "do not replace");
  assert!(state.endpoint(&configured).is_some());
  assert_eq!(state.correlation(&endpoint), correlation);
}

#[cfg(unix)]
#[tokio::test]
async fn a_missing_private_fallback_socket_does_not_block_reconnection_with_forwards() {
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
  let state = State::default();
  let configured = target(Some("builder"));
  state.adopt(&configured, &endpoint, None).unwrap();
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
    Err(RequestError::SshConfig(message)) if message.contains("started outside ctmux")
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
      start_master(
        &configured,
        &endpoint(startup),
        "unused-token",
        &ctl_ipc::socket_path(),
        #[cfg(target_os = "macos")]
        None,
      ),
      Err(RequestError::SshConfig(_))
    ));
  }
}

#[test]
fn reused_endpoints_share_correlation_but_replacement_starts_a_new_one() {
  let state = State::default();
  let endpoint = MasterEndpoint::managed(&target(None));
  let first = state.correlation(&endpoint);
  assert_eq!(state.correlation(&endpoint), first);
  let other = MasterEndpoint {
    control_path: "/different/socket".into(),
    ..endpoint.clone()
  };
  assert_ne!(state.correlation(&other), first);
  state.forget_correlation(&endpoint);
  assert_ne!(state.correlation(&endpoint), first);
}

#[cfg(unix)]
#[test]
fn replacing_a_socket_at_the_same_path_changes_connection_correlation() {
  let path = std::env::temp_dir().join(format!("ctld-correlation-{}", uuid::Uuid::new_v4()));
  let guard = SocketGuard(path);
  let state = State::default();
  let endpoint = MasterEndpoint {
    control_path: guard.0.clone(),
    ..MasterEndpoint::managed(&target(None))
  };
  let pending = state.correlation(&endpoint);
  let socket = std::os::unix::net::UnixListener::bind(&guard.0).unwrap();
  assert_eq!(state.correlation(&endpoint), pending);
  std::fs::remove_file(&guard.0).unwrap();
  let replacement = std::os::unix::net::UnixListener::bind(&guard.0).unwrap();
  assert_ne!(state.correlation(&endpoint), pending);
  drop((socket, replacement));
}
