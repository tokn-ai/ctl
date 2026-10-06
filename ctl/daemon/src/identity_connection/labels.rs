//! Names in authentication requests come from the local connection settings.

use ctl_ipc::SshTarget;

pub(crate) fn save_offer_message(context: &str, names: impl IntoIterator<Item = String>) -> String {
  let mut names: Vec<_> = names.into_iter().collect();
  names.sort();
  names.dedup();
  let names = names.join("\n");
  format!(
    "Save these credentials in Keychain for {context}?\n\n{names}\n\nAuthorize access with Touch ID or your macOS account password. Successful connections can reuse approval for up to 24 hours, ending when you lock or sleep your Mac."
  )
}

pub(crate) fn connection_context(target: &SshTarget) -> String {
  let (configured_user, destination) = target
    .destination
    .rsplit_once('@')
    .map_or((None, target.destination.as_str()), |(user, host)| {
      (Some(user), host)
    });
  let host = target.hostname.as_deref().unwrap_or(destination);
  let host = if host.contains(':') && !host.starts_with('[') {
    format!("[{host}]")
  } else {
    host.to_owned()
  };
  let user = target
    .user
    .as_deref()
    .or(configured_user)
    .map_or_else(String::new, |user| format!("{user}@"));
  let port = target
    .port
    .map_or_else(String::new, |port| format!(":{port}"));
  let alias = target
    .ssh_config_alias
    .as_deref()
    .filter(|alias| *alias != destination && *alias != host)
    .map_or_else(String::new, |alias| format!(" ({alias})"));
  format!("{user}{host}{port}{alias}")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn target() -> SshTarget {
    SshTarget {
      destination: "alice@example.test".into(),
      hostname: None,
      ssh_config_alias: None,
      use_ssh_config_master: None,
      user: None,
      port: None,
      identity_file: None,
      gateways: Vec::new(),
    }
  }

  #[test]
  fn context_names_the_configured_account_endpoint_and_port() {
    let mut target = target();
    assert_eq!(connection_context(&target), "alice@example.test");
    target.user = Some("bob".into());
    target.hostname = Some("host.example.test".into());
    target.port = Some(2222);
    target.ssh_config_alias = Some("work".into());
    assert_eq!(
      connection_context(&target),
      "bob@host.example.test:2222 (work)"
    );
  }

  #[test]
  fn context_disambiguates_ipv6_ports_and_does_not_repeat_an_alias() {
    let mut target = target();
    target.hostname = Some("2001:db8::1".into());
    target.port = Some(22);
    assert_eq!(connection_context(&target), "alice@[2001:db8::1]:22");
    target.destination = "work".into();
    target.hostname = None;
    target.port = None;
    target.ssh_config_alias = Some("work".into());
    assert_eq!(connection_context(&target), "work");
  }

  #[test]
  fn save_consent_names_each_verified_key_and_password_in_stable_order() {
    let message = save_offer_message(
      "alice@example.test",
      [
        "SSH password: gateway".into(),
        "SSH identity passphrase: /keys/work".into(),
        "SSH password: gateway".into(),
      ],
    );
    assert_eq!(
      message,
      concat!(
        "Save these credentials in Keychain for alice@example.test?\n\n",
        "SSH identity passphrase: /keys/work\nSSH password: gateway\n\n",
        "Authorize access with Touch ID or your macOS account password. ",
        "Successful connections can reuse approval for up to 24 hours, ",
        "ending when you lock or sleep your Mac.",
      )
    );
  }
}
