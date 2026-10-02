use std::io::{self, IsTerminal as _};

use ctl_ipc::{VpnConnection, VpnProvider, VpnSettings};
use zeroize::Zeroizing;

pub(super) fn available() -> bool {
  io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub(super) fn create(names: Vec<String>) -> Result<Option<VpnConnection>, super::Error> {
  require_terminal("create")?;
  cliclack::intro("Create a VPN profile")?;
  let result = (|| -> io::Result<VpnConnection> {
    let name: String = cliclack::input("VPN name")
      .validate(move |value: &String| -> Result<(), String> {
        text(value, 256, false)?;
        if names.iter().any(|name| name == value.trim()) {
          return Err("A VPN with this name already exists.".into());
        }
        Ok(())
      })
      .interact()?;
    let provider = cliclack::select("VPN provider")
      .item(
        VpnProvider::Openconnect,
        "OpenConnect",
        "Gateway, username, and password",
      )
      .item(
        VpnProvider::Tailscale,
        "Tailscale",
        "Browser sign-in when started",
      )
      .interact()?;
    let settings = match provider {
      VpnProvider::Openconnect => {
        let url: String = cliclack::input("VPN server")
          .placeholder("https://vpn.example.com")
          .validate(|value: &String| gateway(value))
          .interact()?;
        let username: String = cliclack::input("Username")
          .validate(|value: &String| text(value, 256, false))
          .interact()?;
        let password = Zeroizing::new(
          cliclack::password("Password")
            .validate(|value: &String| text(value, 4096, false))
            .interact()?,
        );
        let auth_method: String = cliclack::input("Authentication group (optional)")
          .required(false)
          .validate(|value: &String| text(value, 256, true))
          .interact()?;
        let target_ip: String = cliclack::input("Connectivity check IPv4 address (optional)")
          .required(false)
          .validate(|value: &String| {
            if value.trim().is_empty() || value.trim().parse::<std::net::Ipv4Addr>().is_ok() {
              Ok(())
            } else {
              Err("Enter an IPv4 address or leave this blank.")
            }
          })
          .interact()?;
        VpnSettings::Openconnect {
          url: url.trim().into(),
          username: username.trim().into(),
          password,
          auth_method: optional(&auth_method),
          target_ip: optional(&target_ip),
        }
      }
      VpnProvider::Tailscale => {
        let hostname: String = cliclack::input("Device name (optional)")
          .required(false)
          .validate(|value: &String| device_name(value))
          .interact()?;
        let accept_routes = cliclack::confirm("Use advertised subnet routes?")
          .initial_value(false)
          .interact()?;
        VpnSettings::Tailscale {
          hostname: optional(&hostname),
          accept_routes,
        }
      }
    };
    Ok(VpnConnection {
      connection_id: uuid::Uuid::new_v4().to_string(),
      name: name.trim().into(),
      settings,
    })
  })();
  cancelled(result)
}

pub(super) fn pick(
  action: &'static str,
  options: Vec<(String, String, String)>,
) -> Result<Option<String>, super::Error> {
  require_terminal(action)?;
  let mut select = cliclack::select(format!("Choose a VPN to {action}"));
  for (id, label, hint) in options {
    select = select.item(id, crate::table::text(&label), crate::table::text(&hint));
  }
  cancelled(select.filter_mode().max_rows(10).interact())
}

pub(super) fn confirm_remove(connection: &VpnConnection) -> Result<bool, super::Error> {
  require_terminal("remove")?;
  let confirmed = cancelled(
    cliclack::confirm(format!(
      "Remove VPN profile {} ({})?",
      crate::table::text(&connection.name),
      crate::table::text(&connection.connection_id)
    ))
    .initial_value(false)
    .interact(),
  )?;
  if confirmed == Some(false) {
    cliclack::outro_cancel("Cancelled. No changes made.")?;
  }
  Ok(confirmed == Some(true))
}

fn require_terminal(action: &'static str) -> Result<(), super::Error> {
  if available() {
    Ok(())
  } else {
    Err(super::Error::TerminalRequired(action))
  }
}

fn cancelled<T>(result: io::Result<T>) -> Result<Option<T>, super::Error> {
  match result {
    Ok(value) => Ok(Some(value)),
    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
      cliclack::outro_cancel("Cancelled. No changes made.")?;
      Ok(None)
    }
    Err(error) => Err(error.into()),
  }
}

fn optional(value: &str) -> Option<String> {
  let value = value.trim();
  (!value.is_empty()).then(|| value.into())
}

fn text(value: &str, maximum: usize, optional: bool) -> Result<(), String> {
  if !optional && value.trim().is_empty() {
    return Err("Enter a value.".into());
  }
  if value.len() > maximum {
    return Err(format!("Use at most {maximum} bytes."));
  }
  if value.chars().any(char::is_control) {
    return Err("Control characters are not allowed.".into());
  }
  Ok(())
}

fn gateway(value: &str) -> Result<(), String> {
  text(value, 2048, false)?;
  let value = value.trim();
  let gateway = value.strip_prefix("https://").unwrap_or(value);
  if value.contains('\\')
    || gateway.contains("://")
    || gateway
      .split('/')
      .next()
      .is_some_and(|host| host.contains('@'))
  {
    return Err("Use an HTTPS gateway without URL credentials or backslashes.".into());
  }
  let address = if value.starts_with("https://") {
    value.to_owned()
  } else {
    format!("https://{value}")
  };
  let valid = url::Url::parse(&address).is_ok_and(|url| {
    url.scheme() == "https"
      && url.host_str().is_some()
      && url.username().is_empty()
      && url.password().is_none()
      && !value.chars().any(char::is_whitespace)
  });
  if valid {
    Ok(())
  } else {
    Err("Enter an HTTPS gateway or bare gateway address without URL credentials.".into())
  }
}

fn device_name(value: &str) -> Result<(), String> {
  let value = value.trim();
  if value.is_empty()
    || (value.len() <= 63
      && value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
      && !value.starts_with('-')
      && !value.ends_with('-'))
  {
    Ok(())
  } else {
    Err("Use 1–63 letters, digits, or interior hyphens, or leave this blank.".into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn gateway_answers_can_be_saved_and_reject_url_normalization_mismatches() {
    for value in [
      "vpn.example.test",
      "vpn.example.test:8443/group",
      "https://vpn.example.test/group?method=password#login",
      "https://[2001:db8::1]:8443/group",
    ] {
      gateway(value).unwrap();
      VpnConnection {
        connection_id: "test-profile".into(),
        name: "Test VPN".into(),
        settings: VpnSettings::Openconnect {
          url: value.into(),
          username: "test-user".into(),
          password: Zeroizing::new("test-password".into()),
          auth_method: None,
          target_ip: None,
        },
      }
      .validate()
      .unwrap();
    }
    for value in [
      "HTTPS://vpn.example.test",
      "http://vpn.example.test",
      "https://vpn.example.test\\group",
      "vpn.example.test\\group",
      "https://user:password@vpn.example.test",
      "https://vpn.example.test?user=@account",
      "https://vpn.example.test/group?redirect=https://other.example.test",
    ] {
      assert!(gateway(value).is_err(), "accepted {value:?}");
    }
  }
}
