//! Provider-specific saved settings, with a reader for legacy `OpenConnect` profiles.

use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnProvider {
  #[default]
  Openconnect,
  Tailscale,
}

/// Intentionally has no Debug implementation: `OpenConnect` contains a password.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum VpnSettings {
  Openconnect {
    url: String,
    username: String,
    password: Zeroizing<String>,
    auth_method: Option<String>,
    target_ip: Option<String>,
  },
  Tailscale {
    hostname: Option<String>,
    #[serde(default)]
    accept_routes: bool,
  },
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct VpnConnection {
  pub connection_id: String,
  pub name: String,
  #[serde(flatten)]
  pub settings: VpnSettings,
}

impl<'de> Deserialize<'de> for VpnConnection {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    struct Tagged {
      connection_id: String,
      name: String,
      #[serde(flatten)]
      settings: VpnSettings,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Legacy {
      connection_id: String,
      name: String,
      url: String,
      username: String,
      password: Zeroizing<String>,
      auth_method: Option<String>,
      target_ip: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
      Tagged(Tagged),
      Legacy(Legacy),
    }
    match Wire::deserialize(deserializer)? {
      Wire::Tagged(value) => Ok(Self {
        connection_id: value.connection_id,
        name: value.name,
        settings: value.settings,
      }),
      Wire::Legacy(value) => Ok(Self {
        connection_id: value.connection_id,
        name: value.name,
        settings: VpnSettings::Openconnect {
          url: value.url,
          username: value.username,
          password: value.password,
          auth_method: value.auth_method,
          target_ip: value.target_ip,
        },
      }),
    }
  }
}

impl VpnConnection {
  #[must_use]
  pub const fn provider(&self) -> VpnProvider {
    match &self.settings {
      VpnSettings::Openconnect { .. } => VpnProvider::Openconnect,
      VpnSettings::Tailscale { .. } => VpnProvider::Tailscale,
    }
  }

  /// Validate without including supplied values in diagnostics.
  ///
  /// # Errors
  /// Returns a field-only diagnostic when a setting is invalid.
  pub fn validate(&self) -> Result<(), String> {
    for (name, value, maximum) in [
      ("Connection ID", self.connection_id.as_str(), 128),
      ("Name", self.name.as_str(), 256),
    ] {
      required_field(name, value, maximum)?;
    }
    match &self.settings {
      VpnSettings::Openconnect {
        url,
        username,
        password,
        auth_method,
        target_ip,
      } => {
        required_field("VPN URL", url, 2048)?;
        required_field("Username", username, 256)?;
        validate_field("Password", password, 4096)?;
        if password.is_empty() {
          return Err("Password is required".into());
        }
        let address = url.strip_prefix("https://").unwrap_or(url);
        if address.contains("://")
          || url.chars().any(char::is_whitespace)
          || address
            .split('/')
            .next()
            .is_none_or(|host| host.is_empty() || host.contains('@'))
        {
          return Err("VPN URL must be an HTTPS gateway or bare gateway address".into());
        }
        if let Some(value) = auth_method {
          validate_field("Authentication method", value, 256)?;
        }
        if let Some(value) = target_ip {
          validate_field("Connectivity target", value, 64)?;
          if !value.is_empty() && value.parse::<std::net::Ipv4Addr>().is_err() {
            return Err("Connectivity target must be an IPv4 address".into());
          }
        }
      }
      VpnSettings::Tailscale { hostname, .. } => {
        if self.connection_id.chars().any(char::is_whitespace) {
          return Err("Connection ID cannot contain whitespace".into());
        }
        if let Some(hostname) = hostname
          && (hostname.is_empty()
            || hostname.len() > 63
            || !hostname
              .bytes()
              .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || hostname.starts_with('-')
            || hostname.ends_with('-'))
        {
          return Err("Device name must be 1–63 letters, digits, or interior hyphens".into());
        }
      }
    }
    Ok(())
  }
}

fn required_field(name: &str, value: &str, maximum: usize) -> Result<(), String> {
  validate_field(name, value, maximum)?;
  if value.trim().is_empty() {
    return Err(format!("{name} is required"));
  }
  Ok(())
}

fn validate_field(name: &str, value: &str, maximum: usize) -> Result<(), String> {
  if value.len() > maximum || value.contains(['\r', '\n', '\0']) {
    return Err(format!(
      "{name} must be at most {maximum} bytes and cannot contain line breaks or NUL"
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn old_profiles_read_as_openconnect_and_write_explicit_provider() {
    let old = serde_json::json!({
      "connection_id":"work", "name":"Work", "url":"https://vpn.example.test",
      "username":"fixture-user", "password":"fixture-secret", "auth_method":null, "target_ip":null
    });
    let connection: VpnConnection = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(connection.provider(), VpnProvider::Openconnect);
    connection.validate().unwrap();
    let mut expected = old;
    expected["provider"] = "openconnect".into();
    assert_eq!(serde_json::to_value(&connection).unwrap(), expected);
    assert!(serde_json::from_value::<VpnConnection>(expected).unwrap() == connection);
  }

  #[test]
  fn legacy_openconnect_keeps_literal_password_validation() {
    for password in ["fixture\tsecret", "   ", "fixture\u{001b}secret"] {
      let value = serde_json::json!({
        "connection_id":"work", "name":"Work", "url":"https://vpn.example.test",
        "username":"fixture-user", "password":password
      });
      let connection: VpnConnection = serde_json::from_value(value).unwrap();
      connection.validate().unwrap();
      let VpnSettings::Openconnect {
        password: stored, ..
      } = connection.settings
      else {
        panic!("legacy provider changed");
      };
      assert_eq!(stored.as_str(), password);
    }
    for password in ["", "fixture\nsecret", "fixture\rsecret", "fixture\0secret"] {
      let connection: VpnConnection = serde_json::from_value(serde_json::json!({
        "connection_id":"work", "name":"Work", "url":"https://vpn.example.test",
        "username":"fixture-user", "password":password
      }))
      .unwrap();
      assert!(connection.validate().is_err());
    }
  }

  #[test]
  fn tailscale_profiles_need_no_secret_and_reject_cross_provider_fields() {
    let value = serde_json::json!({
      "connection_id":"tailnet", "name":"Team", "provider":"tailscale", "hostname":"rmux-test", "accept_routes":true
    });
    let connection: VpnConnection = serde_json::from_value(value.clone()).unwrap();
    connection.validate().unwrap();
    assert_eq!(serde_json::to_value(&connection).unwrap(), value);
    for (field, extra) in [
      ("password", "private-marker"),
      ("provider", "unknown"),
      ("url", "https://vpn.example.test"),
    ] {
      let mut invalid = value.clone();
      invalid[field] = extra.into();
      let error = serde_json::from_value::<VpnConnection>(invalid)
        .err()
        .unwrap();
      assert!(!error.to_string().contains("private-marker"));
    }
    for hostname in [
      "",
      "-device",
      "device-",
      "device.example",
      "device name",
      "device\nname",
    ] {
      let connection = VpnConnection {
        connection_id: "tailnet".into(),
        name: "Team".into(),
        settings: VpnSettings::Tailscale {
          hostname: Some(hostname.into()),
          accept_routes: false,
        },
      };
      assert!(connection.validate().is_err());
    }
  }
}
