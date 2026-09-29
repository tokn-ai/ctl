use super::*;

struct Fixture {
  root: PathBuf,
  paths: ConfigPaths,
}

impl Fixture {
  fn new() -> Self {
    let root = std::env::temp_dir().join(format!("ctl-ssh-endpoint-{}", uuid::Uuid::new_v4()));
    let paths = ConfigPaths {
      home: root.join("home"),
      user_directory: root.join("home/.ssh"),
      system_directory: root.join("system"),
    };
    std::fs::create_dir_all(&paths.user_directory).unwrap();
    std::fs::create_dir_all(&paths.system_directory).unwrap();
    Self { root, paths }
  }

  fn user(&self, contents: &str) {
    std::fs::write(self.paths.user_directory.join("config"), contents).unwrap();
  }

  fn system(&self, contents: &str) {
    std::fs::write(self.paths.system_directory.join("ssh_config"), contents).unwrap();
  }

  fn include(&self, relative: &str, contents: &str) {
    let path = self.paths.user_directory.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
  }

  fn resolve(&self, target: &SshTarget) -> Result<Endpoint, SshReachability> {
    resolve_with_paths(target, &self.paths)
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.root);
  }
}

fn target(destination: &str) -> SshTarget {
  SshTarget {
    destination: destination.into(),
    ssh_config_alias: None,
    use_ssh_config_master: None,
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    gateways: Vec::new(),
  }
}

fn endpoint(host: &str, port: u16) -> Endpoint {
  Endpoint {
    host: host.into(),
    port,
  }
}

fn assert_unsupported(result: Result<Endpoint, SshReachability>) {
  let error = result.unwrap_err();
  assert_eq!(error.state, SshReachabilityState::NotChecked);
  assert_eq!(
    error.reason,
    Some(SshReachabilityReason::UnsupportedConfiguration)
  );
}

#[test]
fn absent_configuration_uses_destination_and_default_port_without_network() {
  let fixture = Fixture::new();
  let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
  listener.set_nonblocking(true).unwrap();
  let mut target = target("127.0.0.1");
  assert_eq!(fixture.resolve(&target).unwrap(), endpoint("127.0.0.1", 22));
  target.port = Some(listener.local_addr().unwrap().port());
  assert_eq!(fixture.resolve(&target).unwrap().port, target.port.unwrap());
  assert_eq!(
    listener.accept().unwrap_err().kind(),
    io::ErrorKind::WouldBlock
  );
}

#[test]
fn alias_and_wildcard_options_use_first_values_before_system_defaults() {
  let fixture = Fixture::new();
  fixture.user("Host demo\n  HostName node.example\n  Port=2201\nHost *\n  HostName ignored.example\n  Port 2202\n");
  fixture.system("Host *\n  Port 2203\n  HostName system.example\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("node.example", 2201)
  );
}

#[test]
fn selected_alias_preserves_host_match_with_explicit_hostname_and_port() {
  let fixture = Fixture::new();
  fixture.user("Host demo\n  HostName config.example\n  Port 2201\nHost explicit.example\n  ProxyCommand blocked\n");
  let mut target = target("demo");
  target.ssh_config_alias = Some("demo".into());
  target.hostname = Some("explicit.example".into());
  target.port = Some(2209);
  assert_eq!(
    fixture.resolve(&target).unwrap(),
    endpoint("explicit.example", 2209)
  );
}

#[test]
fn explicit_hostname_without_selected_alias_changes_host_match() {
  let fixture = Fixture::new();
  fixture.user("Host demo\n  ProxyCommand blocked\nHost explicit.example\n  HostName resolved.example\n  Port 2208\n");
  let mut target = target("demo");
  target.hostname = Some("explicit.example".into());
  assert_eq!(
    fixture.resolve(&target).unwrap(),
    endpoint("resolved.example", 2208)
  );
}

#[test]
fn host_patterns_support_negation_question_marks_and_literal_brackets() {
  let fixture = Fixture::new();
  fixture.user("Host *.example !private.example\n  Port 2201\nHost private.?xample\n  Port 2202\nHost [ab].example\n  Port 2203\nHost *\n  Port 2204\n");
  assert_eq!(
    fixture.resolve(&target("public.example")).unwrap().port,
    2201
  );
  assert_eq!(
    fixture.resolve(&target("private.example")).unwrap().port,
    2202
  );
  assert!(!wildcard_matches(b"a.example", b"[ab].example"));
  assert!(!host_matches("demo", &["!other".into()]).unwrap());
  assert!(wildcard_matches(b"abc.example", b"a*?example"));
}

#[test]
fn included_files_use_lexical_order_and_restore_containing_host_condition() {
  let fixture = Fixture::new();
  fixture.include("parts/20.conf", "Host demo\n  Port 2202\n");
  fixture.include("parts/10.conf", "Host demo\n  Port 2201\nHost unrelated\n");
  fixture.user("Host demo\n  Include parts/*.conf\n  HostName restored.example\nHost unrelated\n  Include bad.conf\n");
  fixture.include("bad.conf", "Host *\n  ProxyCommand blocked\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("restored.example", 2201)
  );
}

#[test]
fn include_paths_support_quotes_home_paths_and_system_relative_paths() {
  let fixture = Fixture::new();
  fixture.user("Include \"parts/with spaces.conf\" ~/.ssh/home.conf\n");
  fixture.include(
    "parts/with spaces.conf",
    "Host demo\n  HostName quoted.example\n",
  );
  fixture.include("home.conf", "Host demo\n  User ignored\n");
  fixture.system("Include defaults.conf\n");
  std::fs::write(
    fixture.paths.system_directory.join("defaults.conf"),
    "Host demo\n  Port 2207\n",
  )
  .unwrap();
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("quoted.example", 2207)
  );
}

#[test]
fn nested_relative_includes_remain_relative_to_ssh_directory() {
  let fixture = Fixture::new();
  fixture.user("Include parts/first.conf\n");
  fixture.include("parts/first.conf", "Include root.conf\n");
  fixture.include("root.conf", "HostName correct.example\n");
  fixture.include("parts/root.conf", "HostName wrong.example\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap().host,
    "correct.example"
  );
}

#[test]
fn hostname_tokens_expand_without_rewriting_later_host_match() {
  let fixture = Fixture::new();
  fixture.user("Host demo\n  HostName %h.example\nHost demo.example\n  ProxyCommand blocked\nHost *\n  Port 2206\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("demo.example", 2206)
  );
}

#[test]
fn proxy_configuration_is_never_bypassed() {
  for config in [
    "ProxyCommand start-vpn %h %p\n",
    "ProxyJump gateway\n",
    "ProxyJump none\nProxyCommand start-vpn %h %p\n",
    "ProxyCommand \"none\"\n",
    "ProxyJump \"none\"\n",
    "ProxyJump gateway\nProxyCommand none\n",
    "Host demo\n ProxyJump gateway\nHost *\n ProxyJump none\n",
  ] {
    let fixture = Fixture::new();
    fixture.user(config);
    assert_unsupported(fixture.resolve(&target("demo")));
  }
}

#[test]
fn disabled_and_nonmatching_proxy_options_do_not_block_direct_check() {
  for config in [
    "ProxyCommand none\nProxyJump gateway\n",
    "ProxyJump none\nProxyJump gateway\n",
    "Host unrelated\n ProxyCommand start-vpn\nHost demo\n Port 2205\n",
  ] {
    let fixture = Fixture::new();
    fixture.user(config);
    assert!(fixture.resolve(&target("demo")).is_ok());
  }
}

#[test]
fn system_proxy_is_respected_unless_user_config_disables_it_first() {
  let fixture = Fixture::new();
  fixture.system("Host *\n ProxyCommand start-vpn %h %p\n");
  assert_unsupported(fixture.resolve(&target("demo")));
  fixture.user("Host demo\n ProxyCommand none\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("demo", 22)
  );
}

#[test]
fn explicit_app_route_takes_precedence_over_config_proxy() {
  let fixture = Fixture::new();
  fixture.user("ProxyCommand start-vpn\nProxyJump gateway\nHostName routed.example\n");
  let mut target = target("demo");
  target.gateways.push(ctld_ipc::SshGateway {
    kind: ctld_ipc::GatewayKind::Vpn,
    vpn: Some(ctld_ipc::VpnGateway {
      connection_id: "fixture".into(),
      socket_path: fixture.root.join("ctld.sock"),
    }),
    destination: "fixture".into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    mode: ctld_ipc::SshGatewayMode::Automatic,
  });
  assert_eq!(
    fixture.resolve(&target).unwrap(),
    endpoint("routed.example", 22)
  );
}

#[test]
fn match_exec_is_not_executed_even_after_unmatched_host() {
  let fixture = Fixture::new();
  let marker = fixture.root.join("should-not-exist");
  fixture.user(&format!(
    "Host unrelated\nMatch exec \"touch '{}'\"\n HostName unexpected.example\n",
    marker.display()
  ));
  assert_unsupported(fixture.resolve(&target("demo")));
  assert!(!marker.exists());
}

#[test]
fn dynamic_routes_bindings_and_include_expansions_are_skipped() {
  for config in [
    "Match all\nPort 22\n",
    "CanonicalizeHostname yes\n",
    "BindAddress 127.0.0.1\n",
    "BindInterface fixture0\n",
    "AddressFamily inet6\n",
    "Include ${CONFIG_ROOT}/*.conf\n",
    "Include %h.conf\n",
    "Include **/config\n",
    "Include ~someone/.ssh/config\n",
    "HostName %n.example\n",
    "UnknownEndpointDirective value\n",
    "RefuseConnection yes\n",
  ] {
    let fixture = Fixture::new();
    fixture.user(config);
    assert_unsupported(fixture.resolve(&target("demo")));
  }
}

#[test]
fn irrelevant_authentication_settings_and_nonmatching_routes_are_ignored() {
  let fixture = Fixture::new();
  fixture.user("IdentityFile ~/.ssh/key\nUser demo\nUseKeychain yes\nServerAliveInterval 30\nHost unrelated\n BindInterface fixture0\n CanonicalizeHostname yes\nHost demo\n Port 2204\n");
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("demo", 2204)
  );
}

#[test]
fn first_disabled_canonicalization_wins() {
  let fixture = Fixture::new();
  fixture.user(
    "CanonicalizeHostname no\nCanonicalizeHostname yes\nAddressFamily any\nAddressFamily inet6\n",
  );
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("demo", 22)
  );
}

#[test]
fn host_matching_preserves_alias_case() {
  let fixture = Fixture::new();
  fixture.user("Host DEMO\n HostName upper.example\n Port 2201\nHost demo\n HostName lower.example\n Port 2202\n");
  assert_eq!(
    fixture.resolve(&target("DEMO")).unwrap(),
    endpoint("upper.example", 2201)
  );
  assert_eq!(
    fixture.resolve(&target("demo")).unwrap(),
    endpoint("lower.example", 2202)
  );
}

#[test]
fn malformed_config_and_recursive_includes_fail_closed() {
  for config in [
    "Port 0\n",
    "Port 22 23\n",
    "HostName \"incomplete\n",
    "Host\n",
    "Include config\n",
    "HostName host#literal\n",
  ] {
    let fixture = Fixture::new();
    fixture.user(config);
    assert_unsupported(fixture.resolve(&target("demo")));
  }
}

#[test]
fn excessive_configuration_and_non_regular_files_are_not_read() {
  let fixture = Fixture::new();
  fixture.user(&"#".repeat(MAX_CONFIG_BYTES + 1));
  assert_unsupported(fixture.resolve(&target("demo")));
  std::fs::remove_file(fixture.paths.user_directory.join("config")).unwrap();
  std::fs::create_dir(fixture.paths.user_directory.join("config")).unwrap();
  assert_unsupported(fixture.resolve(&target("demo")));
}

#[test]
fn username_and_ipv6_destinations_preserve_endpoint() {
  let fixture = Fixture::new();
  fixture.user("Host demo\n Port 2203\n");
  assert_eq!(
    fixture.resolve(&target("user@demo")).unwrap(),
    endpoint("demo", 2203)
  );
  assert_eq!(
    fixture.resolve(&target("user@[::1]")).unwrap(),
    endpoint("::1", 22)
  );
  assert_unsupported(fixture.resolve(&target("ssh://user@demo:2203")));
}

#[test]
fn unreadable_configuration_does_not_claim_unavailability() {
  let fixture = Fixture::new();
  std::fs::write(fixture.paths.user_directory.join("config"), [0xff, 0xfe]).unwrap();
  let error = fixture.resolve(&target("demo")).unwrap_err();
  assert_eq!(error.state, SshReachabilityState::Unknown);
  assert_eq!(error.reason, Some(SshReachabilityReason::CheckFailed));
}
