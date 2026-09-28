use ctld_ipc::{VpnConnection, VpnSettings};
use serde::{Deserialize, Deserializer, Serialize};
use zeroize::Zeroizing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VpnConnectionSummary {
  pub(super) connection_id: String,
  pub(super) name: String,
  #[serde(flatten)]
  pub(super) settings: VpnSettingsSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub(super) enum VpnSettingsSummary {
  Openconnect {
    url: String,
    username: String,
    has_password: bool,
    auth_method: Option<String>,
    target_ip: Option<String>,
  },
  Tailscale {
    hostname: Option<String>,
    accept_routes: bool,
  },
}

impl From<&VpnConnection> for VpnConnectionSummary {
  fn from(connection: &VpnConnection) -> Self {
    let settings = match &connection.settings {
      VpnSettings::Openconnect {
        url,
        username,
        password,
        auth_method,
        target_ip,
      } => VpnSettingsSummary::Openconnect {
        url: url.clone(),
        username: username.clone(),
        has_password: !password.is_empty(),
        auth_method: auth_method.clone(),
        target_ip: target_ip.clone(),
      },
      VpnSettings::Tailscale {
        hostname,
        accept_routes,
      } => VpnSettingsSummary::Tailscale {
        hostname: hostname.clone(),
        accept_routes: *accept_routes,
      },
    };
    Self {
      connection_id: connection.connection_id.clone(),
      name: connection.name.clone(),
      settings,
    }
  }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VpnConnectionsSnapshot {
  pub(super) revision: Option<String>,
  pub(super) connections: Vec<VpnConnectionSummary>,
}

// Password-bearing types deliberately do not implement Debug or Serialize.
pub struct VpnConnectionInput {
  pub(super) connection_id: String,
  pub(super) name: String,
  pub(super) settings: VpnSettingsInput,
}

#[derive(Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum VpnSettingsInput {
  Openconnect {
    url: String,
    username: String,
    password: Option<Zeroizing<String>>,
    auth_method: Option<String>,
    target_ip: Option<String>,
  },
  Tailscale {
    hostname: Option<String>,
    #[serde(default)]
    accept_routes: bool,
  },
}

impl<'de> Deserialize<'de> for VpnConnectionInput {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    struct Tagged {
      connection_id: String,
      name: String,
      #[serde(flatten)]
      settings: VpnSettingsInput,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Legacy {
      connection_id: String,
      name: String,
      url: String,
      username: String,
      password: Option<Zeroizing<String>>,
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
        settings: VpnSettingsInput::Openconnect {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveVpnConnectionRequest {
  pub(super) expected_revision: Option<String>,
  pub(super) connection: VpnConnectionInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteVpnConnectionRequest {
  pub(super) expected_revision: Option<String>,
  pub(super) connection_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectVpnRequest {
  pub(super) connection_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopVpnRequest {
  pub(super) vpn_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenVpnSignInRequest {
  pub(super) vpn_id: String,
}
