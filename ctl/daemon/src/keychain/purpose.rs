//! Human-readable reasons contain only trusted connection/file metadata.

use ctld_ipc::SshTarget;
use ctld_ipc::credentials::StoredCredential;

pub(super) fn text(value: &str) -> String {
  let clean: String = value
    .chars()
    .filter(|character| {
      !character.is_control()
        && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    })
    .collect();
  if clean.len() <= 1024 {
    return clean;
  }
  let prefix: String = clean
    .chars()
    .scan(0, |bytes, character| {
      *bytes += character.len_utf8();
      (*bytes <= 768).then_some(character)
    })
    .collect();
  let suffix: String = clean
    .chars()
    .rev()
    .scan(0, |bytes, character| {
      *bytes += character.len_utf8();
      (*bytes <= 253).then_some(character)
    })
    .collect::<String>()
    .chars()
    .rev()
    .collect();
  format!("{prefix}…{suffix}")
}

pub(super) fn connection(target: &SshTarget) -> String {
  endpoint(
    &target.destination,
    target.hostname.as_deref(),
    target.user.as_deref(),
    target.port,
  )
}

fn endpoint(
  destination: &str,
  hostname: Option<&str>,
  user: Option<&str>,
  port: Option<u16>,
) -> String {
  let (configured_user, destination) = destination
    .rsplit_once('@')
    .map_or((None, destination), |(user, host)| (Some(user), host));
  let host = text(hostname.unwrap_or(destination));
  let host = if host.contains(':') && !host.starts_with('[') {
    format!("[{host}]")
  } else {
    host
  };
  let user = user
    .or(configured_user)
    .map_or_else(String::new, |user| format!("{}@", text(user)));
  let port = port.map_or_else(String::new, |port| format!(":{port}"));
  text(&format!("{user}{host}{port}"))
}

pub(super) fn credential(target: &SshTarget, prompt: &str) -> String {
  let lowered = prompt.to_lowercase();
  let kind = if lowered.starts_with("enter passphrase for key") {
    "SSH key passphrase"
  } else if lowered.contains("password:") {
    "SSH password"
  } else {
    "SSH credential"
  };
  let mut destination = connection(target);
  for gateway in &target.gateways {
    if gateway.kind != ctld_ipc::GatewayKind::Ssh {
      continue;
    }
    let trusted = endpoint(
      &gateway.destination,
      gateway.hostname.as_deref(),
      gateway.user.as_deref(),
      None,
    );
    // Remote text can select only an already configured endpoint. Never turn
    // arbitrary keyboard-interactive text into a trusted authentication reason.
    if prompt.trim() == format!("{trusted}'s password:") {
      destination = endpoint(
        &gateway.destination,
        gateway.hostname.as_deref(),
        gateway.user.as_deref(),
        gateway.port,
      );
      destination.push_str(" (SSH gateway)");
      break;
    }
  }
  let id = super::digest(prompt.as_bytes());
  format!("{kind} for {destination} (credential {})", &id[..8])
}

pub(super) fn stored_credential(credential: &StoredCredential) -> String {
  let mut description = text(&credential.name);
  if let Some(target) = &credential.target {
    description.push_str(" for ");
    if let Some(account) = &credential.account {
      description.push_str(&text(account));
      description.push('@');
      description.push_str(&text(
        target.rsplit_once('@').map_or(target, |(_, host)| host),
      ));
    } else {
      description.push_str(&text(target));
    }
  }
  let account_id = credential
    .credential_id
    .rsplit_once(':')
    .map_or(credential.credential_id.as_str(), |(_, account)| account);
  let short_id: String = account_id.chars().take(8).collect();
  format!("{description} (credential {short_id})")
}

pub(super) fn identity(action: &str, path: &str, context: Option<&str>) -> String {
  let mut reason = format!("{action} SSH identity passphrase for {}", text(path));
  if let Some(context) = context {
    reason.push_str(" to connect to ");
    reason.push_str(&text(context));
  }
  reason
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hostname_resolution_preserves_the_configured_account() {
    assert_eq!(
      endpoint(
        "alice@alias",
        Some("host.example.invalid"),
        None,
        Some(2222)
      ),
      "alice@host.example.invalid:2222",
    );
    assert_eq!(
      endpoint("alice@alias", Some("2001:db8::1"), Some("bob"), Some(22)),
      "bob@[2001:db8::1]:22",
    );
  }

  fn target() -> SshTarget {
    SshTarget {
      destination: "fixture-alias".into(),
      ssh_config_alias: None,
      use_ssh_config_master: None,
      hostname: Some("host.example.invalid".into()),
      user: Some("alice".into()),
      port: Some(2222),
      identity_file: None,
      gateways: Vec::new(),
    }
  }

  #[test]
  fn credentials_name_configured_account_endpoint_and_distinct_exact_item() {
    let target = target();
    assert_eq!(connection(&target), "alice@host.example.invalid:2222");
    let first = credential(&target, "Password:");
    let second = credential(&target, "Untrusted remote claim Password:");
    assert!(first.starts_with("SSH password for alice@host.example.invalid:2222 (credential "));
    assert_ne!(first, second);
    assert!(!second.contains("Untrusted remote claim"));
  }

  #[test]
  fn only_a_configured_gateway_can_name_a_gateway_password() {
    let mut target = target();
    target.gateways.push(ctld_ipc::SshGateway {
      kind: ctld_ipc::GatewayKind::Ssh,
      vpn: None,
      destination: "gateway-alias".into(),
      hostname: Some("jump.example.invalid".into()),
      user: Some("bob".into()),
      port: Some(2200),
      identity_file: None,
      mode: ctld_ipc::SshGatewayMode::Automatic,
    });
    let reason = credential(&target, "bob@jump.example.invalid's password: ");
    assert!(reason.contains("bob@jump.example.invalid:2200 (SSH gateway)"));
    let arbitrary = credential(&target, "mallory@unconfigured.invalid's password:");
    assert!(arbitrary.contains("alice@host.example.invalid:2222"));
    assert!(!arbitrary.contains("unconfigured.invalid"));
  }

  #[test]
  fn stored_credential_account_is_not_duplicated() {
    let credential = StoredCredential {
      credential_id: "fixture".into(),
      scope_id: "fixture".into(),
      name: "SSH password".into(),
      kind: ctld_ipc::credentials::CredentialKind::SshPassword,
      target: Some("alice@host.example.invalid".into()),
      account: Some("alice".into()),
      key_name: None,
      created_at_ms: None,
      updated_at_ms: None,
    };
    assert_eq!(
      stored_credential(&credential),
      "SSH password for alice@host.example.invalid (credential fixture)"
    );
  }

  #[test]
  fn identity_reasons_name_the_action_file_and_connection() {
    let reason = identity(
      "Read",
      "/fixture/id_ed25519",
      Some("alice@example.invalid:2222"),
    );
    assert_eq!(
      reason,
      "Read SSH identity passphrase for /fixture/id_ed25519 to connect to alice@example.invalid:2222"
    );
  }

  #[test]
  fn reasons_remove_controls_and_direction_overrides() {
    assert_eq!(
      text("alice\n@\u{202e}example.invalid"),
      "alice@example.invalid"
    );
    assert_eq!(text(&"x".repeat(2048)).len(), 1024);
    let unicode = text(&format!("{}id_ed25519", "钥".repeat(2048)));
    assert!(unicode.len() <= 1024);
    assert!(unicode.ends_with("id_ed25519"));
  }
}
