use super::*;
use std::sync::{Arc, Mutex};

#[test]
fn proxyjump_parsing_preserves_user_port_and_ipv6_without_accepting_commands_or_credentials() {
  let first = jump_target("alice@jump.test:2200").unwrap();
  assert_eq!(first.destination, "jump.test");
  assert_eq!(first.user.as_deref(), Some("alice"));
  assert_eq!(first.port, Some(2200));
  let ipv6 = jump_target("bob@[::1]:2201").unwrap();
  assert_eq!(ipv6.destination, "[::1]");
  assert_eq!(ipv6.port, Some(2201));
  for invalid in [
    "none",
    "",
    "command host",
    "user:secret@host",
    "host/path",
    "host?command",
    "host#command",
    "%h",
    "-option",
  ] {
    assert!(jump_target(invalid).is_none(), "{invalid}");
  }
}

#[tokio::test]
async fn resolves_explicit_and_configured_jump_chains_once_and_stops_cycles() {
  let mut target = jump_target("destination.test").unwrap();
  target.gateways.push(SshGateway {
    kind: GatewayKind::Ssh,
    vpn: None,
    destination: "jump-a.test".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctld_ipc::SshGatewayMode::Automatic,
  });
  let queried = Arc::new(Mutex::new(Vec::new()));
  let recorded = Arc::clone(&queried);
  let resolved = resolve_with(&target, move |target| {
    recorded.lock().unwrap().push(target.destination.clone());
    let text = match target.destination.as_str() {
      "destination.test" => "proxyjump jump-a.test,jump-b.test\nidentityfile /keys/destination\n",
      "jump-a.test" => "proxyjump destination.test\nidentityfile /keys/a\n",
      "jump-b.test" => "proxyjump jump-c.test\nidentityfile /keys/b\n",
      "jump-c.test" => "identityfile /keys/c\nidentityagent /agents/special\n",
      _ => panic!("unexpected configuration lookup"),
    };
    std::future::ready(Ok(parse_configuration(text)))
  })
  .await
  .unwrap();
  assert_eq!(
    *queried.lock().unwrap(),
    [
      "destination.test",
      "jump-a.test",
      "jump-b.test",
      "jump-c.test"
    ]
  );
  assert_eq!(resolved.gateways.len(), 3);
  assert_eq!(resolved.destination.identity_files, ["/keys/destination"]);
  assert!(resolved.gateways[0].inherits_agent);
  assert!(!resolved.gateways[2].inherits_agent);
}

#[tokio::test]
async fn recursive_gateway_discovery_is_bounded_even_for_an_infinite_config_chain() {
  let target = jump_target("node-0.test").unwrap();
  let queried = Arc::new(Mutex::new(0));
  let recorded = Arc::clone(&queried);
  let resolved = resolve_with(&target, move |_| {
    let mut count = recorded.lock().unwrap();
    *count += 1;
    std::future::ready(Ok(parse_configuration(&format!(
      "proxyjump node-{count}.test\n"
    ))))
  })
  .await
  .unwrap();
  assert_eq!(*queried.lock().unwrap(), MAX_GATEWAYS + 1);
  assert_eq!(resolved.gateways.len(), MAX_GATEWAYS);
}

#[test]
fn agent_mutation_preferences_keep_the_existing_agent_behavior() {
  for value in ["yes", "ask", "confirm", "300"] {
    let config = parse_configuration(&format!("addkeystoagent {value}\n"));
    assert!(config.mutates_agent && config.agent_disabled);
  }
  for value in ["no", "false"] {
    let config = parse_configuration(&format!("addkeystoagent {value}\nforwardagent yes\n"));
    assert!(!config.mutates_agent && !config.agent_disabled);
  }
}

#[test]
fn preferred_authentication_without_publickey_disables_local_identity_candidates() {
  for methods in [
    "password",
    "keyboard-interactive",
    "keyboard-interactive,password",
    "hostbased,gssapi-with-mic",
    "not-publickey",
  ] {
    for configuration in [
      format!("identityagent /agents/fixture\npreferredauthentications {methods}\n"),
      format!("preferredauthentications {methods}\nidentityagent /agents/fixture\n"),
    ] {
      let config = parse_configuration(&format!("identityfile /keys/fixture\n{configuration}"));
      assert_eq!(
        config.publickey_authentication,
        PublicKeyAuthentication::Disabled,
        "{methods}"
      );
      assert!(!config.agent_disabled, "{methods}");
      assert_eq!(config.paths, ["/keys/fixture"]);
      assert_eq!(config.agent, Some(PathBuf::from("/agents/fixture")));
      assert!(!config.inherits_agent);
    }
  }
}

#[test]
fn preferred_authentication_with_publickey_preserves_ordered_fallback_and_default_policy() {
  for preference in [
    "",
    "preferredauthentications publickey\n",
    "preferredauthentications password,publickey\n",
    "preferredauthentications publickey,password\n",
    "preferredauthentications keyboard-interactive,publickey,password\n",
    "preferredauthentications gssapi-with-mic,hostbased,publickey,keyboard-interactive,password\n",
  ] {
    let config = parse_configuration(&format!("identityfile /keys/fixture\n{preference}"));
    assert!(!config.agent_disabled, "{preference}");
    assert_eq!(
      config.publickey_authentication,
      PublicKeyAuthentication::Enabled,
      "{preference}"
    );
    assert!(config.inherits_agent);
  }
  for restriction in ["pubkeyauthentication no", "pubkeyauthentication false"] {
    let config = parse_configuration(&format!(
      "{restriction}\npreferredauthentications password,publickey\n"
    ));
    assert_eq!(
      config.publickey_authentication,
      PublicKeyAuthentication::Disabled,
      "{restriction}"
    );
    assert!(!config.agent_disabled, "{restriction}");
  }
  let config = parse_configuration("identityagent none\npreferredauthentications publickey\n");
  assert!(config.agent_disabled);
  assert_eq!(
    config.publickey_authentication,
    PublicKeyAuthentication::Enabled
  );
}

#[tokio::test]
async fn preferred_authentication_policy_remains_scoped_to_each_gateway() {
  let target = jump_target("destination.test").unwrap();
  let resolved = resolve_with(&target, |target| {
    let configuration = match target.destination.as_str() {
      "destination.test" => "preferredauthentications password\nproxyjump inherited.test,disabled.test,explicit.test\n",
      "inherited.test" => "preferredauthentications password,publickey\nidentityfile /keys/inherited\n",
      "disabled.test" => "preferredauthentications keyboard-interactive\nidentityfile /keys/disabled\n",
      "explicit.test" => "preferredauthentications publickey\nidentityagent /agents/gateway\nidentityfile /keys/explicit\n",
      _ => panic!("unexpected configuration lookup"),
    };
    std::future::ready(Ok(parse_configuration(configuration)))
  })
  .await
  .unwrap();
  assert_eq!(
    resolved.destination.publickey_authentication,
    PublicKeyAuthentication::Disabled
  );
  assert!(!resolved.destination.agent_disabled);
  assert!(resolved.destination.inherits_agent);
  assert_eq!(resolved.gateways.len(), 3);
  assert!(!resolved.gateways[0].agent_disabled);
  assert_eq!(
    resolved.gateways[0].publickey_authentication,
    PublicKeyAuthentication::Enabled
  );
  assert!(resolved.gateways[0].inherits_agent);
  assert_eq!(
    resolved.gateways[1].publickey_authentication,
    PublicKeyAuthentication::Disabled
  );
  assert!(!resolved.gateways[1].agent_disabled);
  assert!(resolved.gateways[1].inherits_agent);
  assert!(!resolved.gateways[2].agent_disabled);
  assert_eq!(
    resolved.gateways[2].publickey_authentication,
    PublicKeyAuthentication::Enabled
  );
  assert!(!resolved.gateways[2].inherits_agent);
}

#[test]
fn explicit_gateway_configuration_matches_the_actual_effective_destination() {
  let gateway = SshGateway {
    kind: GatewayKind::Ssh,
    vpn: None,
    destination: "configured-alias".into(),
    hostname: Some("effective.test".into()),
    user: Some("alice".into()),
    port: Some(2200),
    identity_file: None,
    mode: ctld_ipc::SshGatewayMode::Automatic,
  };
  let target = gateway_target(&gateway).unwrap();
  assert_eq!(target.destination, "effective.test");
  assert_eq!(target.user.as_deref(), Some("alice"));
  assert_eq!(target.port, Some(2200));
  assert!(target.identity_file.is_none());
  // Inline gateway keys are rejected by the existing transport contract; saved
  // gateway hosts with explicit keys are authenticated separately as targets.
  let invalid = SshGateway {
    identity_file: Some("/keys/gateway".into()),
    ..gateway
  };
  assert!(crate::invalid_gateway(&invalid));
}

#[tokio::test]
async fn our_master_launch_passes_its_prepared_agent_to_openssh_proxy_children() {
  let directory = PathBuf::from("/tmp").join(format!(
    "ctld-identity-env-{}",
    uuid::Uuid::new_v4().simple()
  ));
  std::fs::create_dir(&directory).unwrap();
  let original = directory.join("synthetic-original-agent");
  let proxy = super::super::agent_proxy::AgentProxy::start(
    &directory.join("synthetic-local-agent"),
    Some(&original),
  )
  .unwrap();
  let socket = proxy.socket_path().to_owned();
  let prepared = super::super::PreparedIdentities {
    proxy: Some(proxy),
    ..super::super::PreparedIdentities::default()
  };
  let output = directory.join("observed-agent");
  let script = directory.join("proxy.sh");
  // Keep the SSH stdout pipe open until the observation is written. Redirecting
  // the top-level `exec printf` closes that pipe before printf runs, allowing
  // SSH to see EOF and terminate the child while its output file is still empty.
  std::fs::write(
    &script,
    "printf '%s' \"$SSH_AUTH_SOCK\" > \"$CTLD_TEST_AGENT_OUTPUT\"\nexit 0\n",
  )
  .unwrap();
  let mut command = Command::new("/usr/bin/ssh");
  command
    .args([
      "-F",
      "/dev/null",
      "-o",
      "IdentityFile=none",
      "-o",
      "BatchMode=yes",
    ])
    .env("SSH_AUTH_SOCK", &original);
  prepared.append_options(&mut command);
  let result = tokio::time::timeout(
    Duration::from_secs(3),
    command
      .args([
        "-o",
        "ProxyCommand=/bin/sh \"$CTLD_TEST_AGENT_SCRIPT\"",
        "--",
        "example.test",
      ])
      .env("CTLD_TEST_AGENT_OUTPUT", &output)
      .env("CTLD_TEST_AGENT_SCRIPT", &script)
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .kill_on_drop(true)
      .status(),
  )
  .await;
  let observed = std::fs::read_to_string(&output);
  std::fs::remove_dir_all(directory).unwrap();
  assert!(result.is_ok());
  assert_eq!(observed.unwrap(), socket.to_str().unwrap());
}
