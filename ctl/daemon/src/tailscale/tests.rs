use super::*;

fn config(id: &str) -> Config {
  Config::from_connection(&VpnConnection {
    connection_id: id.into(),
    name: "Test tailnet".into(),
    settings: VpnSettings::Tailscale {
      hostname: None,
      accept_routes: true,
    },
  })
  .unwrap()
}

fn status(json: &str) -> VpnStatus {
  serde_json::from_str::<BackendStatus>(json)
    .unwrap()
    .status("container", 49152)
}

#[test]
fn backend_transitions_remove_stale_endpoints_and_expose_only_safe_login_links() {
  let login =
    status(r#"{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/example"}"#);
  assert_eq!(login.state, VpnState::Starting);
  assert!(!login.running);
  assert!(login.endpoint.is_none());
  assert_eq!(
    login.auth_url.as_deref(),
    Some("https://login.tailscale.com/a/example")
  );
  let ready = status(
    r#"{"BackendState":"Running","AuthURL":"https://login.tailscale.com/a/stale","Self":{"UserID":1,"HostName":"test-device"},"User":{"1":{"LoginName":"user@example.test"}},"CurrentTailnet":{"Name":"test-tailnet"}}"#,
  );
  assert_eq!(ready.state, VpnState::Connected);
  assert_eq!(ready.endpoint.as_deref(), Some("socks5h://127.0.0.1:49152"));
  assert_eq!(ready.hostname.as_deref(), Some("test-device"));
  assert_eq!(ready.tailnet.as_deref(), Some("test-tailnet"));
  assert_eq!(ready.username.as_deref(), Some("user@example.test"));
  assert!(ready.auth_url.is_none());
  for json in [
    r#"{"BackendState":"NeedsLogin","AuthURL":"https://evil.example.test/a/private"}"#,
    r#"{"BackendState":"NeedsLogin","AuthURL":"https://login.tailscale.com/a/secret?token=private"}"#,
    r#"{"BackendState":"NeedsMachineAuth","Health":["private diagnostic"]}"#,
    r#"{"BackendState":"Stopped","Health":["private diagnostic"]}"#,
  ] {
    let result = status(json);
    assert!(!result.running);
    assert!(result.endpoint.is_none());
    assert!(result.auth_url.is_none());
    assert!(!serde_json::to_string(&result).unwrap().contains("private"));
  }
}

#[test]
fn identity_names_are_stable_private_and_scoped_to_profile_and_user() {
  let first = container_name(Path::new("/users/test"), "first");
  assert_eq!(first, container_name(Path::new("/users/test"), "first"));
  assert_ne!(first, container_name(Path::new("/users/test"), "second"));
  assert_ne!(first, container_name(Path::new("/users/other"), "first"));
  assert!(!first.contains("first"));
  assert!(!first.contains("test"));
  assert_eq!(config("first").hostname, config("first").hostname);
}

#[test]
fn docker_and_podman_inspect_json_verify_the_lease_without_template_field_names() {
  for id_key in ["Id", "ID"] {
    let bytes = format!(
      r#"[{{"{id_key}":"owned-container","Config":{{"Labels":{{"io.ctl.lease":"owner-token"}}}},"NetworkSettings":{{"Ports":{{"1080/tcp":[{{"HostIp":"127.0.0.1","HostPort":"49152"}}]}}}}}}]"#
    );
    let container = parse_owned_container(bytes.as_bytes(), "owner-token").unwrap();
    assert_eq!(container.id, "owned-container");
    assert_eq!(
      parse_published_port(&serde_json::to_vec(&container.network.unwrap().ports).unwrap())
        .unwrap(),
      49152
    );
    assert_eq!(
      parse_owned_container(bytes.as_bytes(), "another-owner")
        .err()
        .unwrap()
        .kind(),
      io::ErrorKind::PermissionDenied
    );
  }
  let created = parse_owned_container(br#"[{"Id":"created-container","Config":{"Labels":{"io.ctl.lease":"owner-token"}},"NetworkSettings":null}]"#, "owner-token").unwrap();
  assert_eq!(created.id, "created-container");
  assert!(created.network.is_none());
  for bytes in [
    b"[]".as_slice(),
    br#"[{"Id":"owned-container","Config":{"Labels":null},"NetworkSettings":{"Ports":null}}]"#,
    br#"{"Id":"owned-container"}"#,
  ] {
    assert!(parse_owned_container(bytes, "owner-token").is_err());
  }
}

#[cfg(unix)]
mod engine;
