use super::*;
use std::fmt::Write as _;

#[test]
fn effective_identity_paths_are_bounded_and_unresolved_tokens_are_not_guessed() {
  let configuration = parse_configuration(concat!(
    "hostname example.test\nuser alice\n",
    "identityfile /keys/%h/%r\n",
    "identityfile /keys/%%literal\n",
    "identityfile /keys/%x\n",
    "identityfile none\n",
    "identityfile ${UNRESOLVED}/key\n",
    "identityagent none\n",
  ));
  assert_eq!(
    configuration.paths,
    ["/keys/example.test/alice", "/keys/%literal"]
  );
  assert!(configuration.agent_disabled);
  assert!(configuration.agent.is_none());
  let mut output = String::new();
  for index in 0..MAX_IDENTITIES + 20 {
    writeln!(output, "identityfile /keys/{index}").unwrap();
  }
  assert_eq!(parse_configuration(&output).paths.len(), MAX_IDENTITIES);
}

#[test]
fn a_configured_agent_path_is_preserved_for_fallback() {
  let configuration = parse_configuration("identityagent /private/socket with spaces\n");
  assert_eq!(
    configuration.agent,
    Some(PathBuf::from("/private/socket with spaces"))
  );
  assert!(!configuration.agent_disabled);
}

#[test]
fn only_a_complete_openssh_key_prompt_selects_a_candidate_path() {
  assert_eq!(
    prompt_path("Enter passphrase for key '/keys/work': "),
    Some("/keys/work".into())
  );
  for prompt in [
    "Enter passphrase for key '/keys/work'",
    "Enter passphrase for key ''': incorrect",
    "Password: Enter passphrase for key '/keys/work':",
    "Enter passphrase for key '/keys/work\n':",
  ] {
    assert!(prompt_path(prompt).is_none());
  }
  assert!(is_key_prompt("Enter passphrase for key '/keys/work':"));
  assert!(is_key_prompt("enter passphrase for key '/keys/work':"));
  assert!(!is_key_prompt("alice@example.test's password:"));
}

#[tokio::test]
async fn a_remote_key_prompt_cannot_load_or_save_a_key_outside_the_prepared_identity_set() {
  let mut prepared = PreparedIdentities::default();
  let mut captured = HashMap::from([
    (
      "Enter passphrase for key '/unconfigured/identity':".into(),
      Zeroizing::new("candidate".into()),
    ),
    (
      "Enter passphrase for key malformed".into(),
      Zeroizing::new("candidate".into()),
    ),
    (
      "alice@example.test's password:".into(),
      Zeroizing::new("login secret".into()),
    ),
  ]);
  assert!(prepared.verify_captured(&mut captured).await.is_empty());
  assert!(prepared.agent.is_none());
  assert_eq!(captured.len(), 1);
  assert!(captured.contains_key("alice@example.test's password:"));
}

#[tokio::test]
async fn a_remote_key_prompt_always_requests_user_input_instead_of_keychain_autofill() {
  let target = SshTarget {
    destination: "synthetic-host".into(),
    hostname: Some("example.test".into()),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    user: None,
    port: None,
    identity_file: Some(PathBuf::from("/keys/work key")),
    gateways: Vec::new(),
  };
  let (mut client, mut server) = ctld_ipc::Stream::pair().unwrap();
  let (response, response_rx) = tokio::sync::oneshot::channel();
  let worker = tokio::spawn(async move {
    let mut attempted = HashSet::new();
    let mut captured = HashMap::new();
    crate::answer_prompt(
      &mut server,
      &target,
      crate::PromptRequest {
        message: "Enter passphrase for key '/keys/work key':".into(),
        confirm: false,
        response,
      },
      &mut attempted,
      &mut captured,
    )
    .await
    .unwrap();
    assert!(attempted.is_empty());
    assert_eq!(captured.len(), 1);
  });
  let Some(ctld_ipc::ServerMessage::Prompt {
    prompt_id,
    kind: ctld_ipc::PromptKind::Secret,
    ..
  }) = ctld_ipc::read_frame(&mut client).await.unwrap()
  else {
    panic!("key prompts must ask the user");
  };
  ctld_ipc::write_frame(
    &mut client,
    &ctld_ipc::ClientMessage::PromptResponse {
      prompt_id,
      response: Some(Zeroizing::new("user supplied".into())),
    },
  )
  .await
  .unwrap();
  assert_eq!(
    response_rx.await.unwrap().unwrap().as_str(),
    "user supplied"
  );
  worker.await.unwrap();
}

#[tokio::test]
async fn public_identity_hints_preserve_the_effective_default_key_fallbacks() {
  let proxy = AgentProxy::start(Path::new("/missing/synthetic-agent"), None).unwrap();
  let public = proxy.write_public_key("ssh-ed25519 cHVibGlj", 0).unwrap();
  let prepared = PreparedIdentities {
    proxy: Some(proxy),
    public_files: vec![("~/.ssh/id_rsa".into(), public.clone())],
    fallback_files: vec!["~/.ssh/id_rsa".into(), "~/.ssh/id_ed25519".into()],
    ..PreparedIdentities::default()
  };
  let mut command = Command::new("ssh");
  prepared.append_options(&mut command);
  let arguments: Vec<_> = command
    .as_std()
    .get_args()
    .map(|arg| arg.to_string_lossy().into_owned())
    .collect();
  assert_eq!(
    &arguments[2..],
    [
      "-i",
      "~/.ssh/id_rsa",
      "-i",
      public.to_str().unwrap(),
      "-i",
      "~/.ssh/id_ed25519"
    ]
  );
}

#[tokio::test]
async fn cancellation_drops_the_temporary_agent_view_before_returning() {
  let lifecycle = Arc::new(crate::TargetLifecycle::default());
  let mut attempt = lifecycle.attempt();
  let proxy = AgentProxy::start(Path::new("/missing/synthetic-agent"), None).unwrap();
  let socket = proxy.socket_path().to_owned();
  let (started, ready) = tokio::sync::oneshot::channel();
  let worker = tokio::spawn(async move {
    attempt
      .run(async move {
        let _prepared = PreparedIdentities {
          proxy: Some(proxy),
          ..PreparedIdentities::default()
        };
        started.send(()).unwrap();
        std::future::pending::<()>().await;
      })
      .await
  });
  ready.await.unwrap();
  assert!(socket.exists());
  lifecycle.pause();
  assert!(matches!(
    worker.await.unwrap(),
    Err(crate::RequestError::HostDisconnected)
  ));
  assert!(!socket.exists());
}

#[test]
fn disabled_public_key_authentication_and_unresolved_agent_paths_remain_under_openssh_control() {
  for config in [
    "identityfile /keys/id\npubkeyauthentication no\n",
    "identityagent /agents/%x\nidentityfile /keys/id\n",
  ] {
    assert!(parse_configuration(config).agent_disabled);
  }
}

#[test]
fn truncated_prompts_prioritize_configured_prefixes_and_never_supply_new_identity_paths() {
  let path = format!("/keys/{}", "long-name".repeat(20));
  let paths = ["/keys/other", path.as_str()];
  assert_eq!(candidate_order(Some(&path[..100]), &paths), [1, 0]);
  assert_eq!(candidate_order(Some("/unconfigured/key"), &paths), [0, 1]);
  assert_eq!(candidate_order(None, &paths), [0, 1]);
  let many = vec!["/keys/synthetic"; MAX_IDENTITIES + 4];
  assert_eq!(candidate_order(None, &many).len(), MAX_IDENTITIES);
}

#[tokio::test]
async fn temporary_agent_authentication_never_changes_the_forced_disabled_forwarding_policy() {
  let proxy = AgentProxy::start(Path::new("/missing/synthetic-agent"), None).unwrap();
  let prepared = PreparedIdentities {
    proxy: Some(proxy),
    ..PreparedIdentities::default()
  };
  let target = SshTarget {
    destination: "example.test".into(),
    hostname: None,
    ssh_config_alias: None,
    use_ssh_config_master: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  };
  for shared in [false, true] {
    let mut endpoint = crate::MasterEndpoint::managed(&target);
    endpoint.shared = shared;
    let command = crate::master_command_with_identities(&target, &endpoint, Some(&prepared));
    let options: Vec<_> = command
      .as_std()
      .get_args()
      .map(|arg| arg.to_string_lossy().into_owned())
      .collect();
    assert_eq!(
      options
        .iter()
        .find(|arg| arg.starts_with("ForwardAgent="))
        .map(String::as_str),
      Some("ForwardAgent=no")
    );
  }
}
