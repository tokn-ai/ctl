use super::*;

#[test]
fn update_commands_preserve_the_route_and_quote_each_selector() {
  assert_eq!(
    update_command(Some("work"), None, Some("direct")).as_deref(),
    Some("ctlup update --host='work' --method='direct'")
  );
  assert_eq!(
    update_command(Some("office box"), None, None).as_deref(),
    Some("ctlup update --host='office box'")
  );
  assert_eq!(
    update_command(None, Some("person@ssh-alias"), None).as_deref(),
    Some("ctlup update --destination='person@ssh-alias'")
  );
  assert_eq!(
    update_command(Some("-lab's box"), None, Some("lab;route")).as_deref(),
    Some("ctlup update --host='-lab'\\''s box' --method='lab;route'")
  );
  assert!(update_command(Some("work"), None, Some("unsafe\nmethod")).is_none());
  assert!(update_command(Some("work\u{202e}"), None, None).is_none());
  assert!(update_command(None, Some("ssh://user:secret@host"), None).is_none());
}

#[test]
fn only_known_unix_agent_failures_request_an_update() {
  let command = "ctlup update --host='work'";
  for error in [
    CoreError::AgentNotFound,
    CoreError::IdentityUnsupported,
    CoreError::BrokerService(ctl_client::connection::Error::Daemon {
      code: "ctl_agent_not_found".into(),
      message: String::new(),
    }),
    CoreError::BrokerService(ctl_client::connection::Error::Daemon {
      code: "ctl_agent_update_required".into(),
      message: String::new(),
    }),
    CoreError::UnsupportedSshProtocol {
      marker: "ctl-ssh-v2".into(),
    },
    CoreError::UnsupportedSshProtocol {
      marker: "ctl-ssh-v3".into(),
    },
  ] {
    let hint = update_required(&error, RemotePlatform::Unix, Some(command)).unwrap();
    assert_eq!(hint.to_string().lines().last(), Some(command));
    assert!(update_required(&error, RemotePlatform::Windows, Some(command)).is_none());
  }
  for error in [
    CoreError::InvalidSshPreface("banner".into()),
    CoreError::UnsupportedSshProtocol {
      marker: "ctl-ssh-v4".into(),
    },
  ] {
    assert!(update_required(&error, RemotePlatform::Unix, Some(command)).is_none());
  }
}

#[test]
fn missing_exact_route_gives_guidance_without_guessing_a_target() {
  let hint = update_required(&CoreError::AgentNotFound, RemotePlatform::Unix, None)
    .unwrap()
    .to_string();
  assert!(hint.contains("same SSH account and route"));
  assert!(!hint.contains("--host="));
  assert!(!hint.contains("--destination="));
}
