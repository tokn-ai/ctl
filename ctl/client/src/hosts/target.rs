use super::HostError;
use serde::{Deserialize, Serialize};

/// Explicit endpoint identity carried by every operation that opens a stream.
///
/// The SSH value is an OpenSSH destination or configured host alias. Optional
/// app-local settings are passed as fixed SSH arguments; arbitrary options,
/// remote commands, credentials, and forwarding configuration remain absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshGatewayModeDto {
  Automatic,
  NativeOnly,
  AgentRelayOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshGatewayRouteStepDto {
  pub gateway_id: String,
  pub mode: SshGatewayModeDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshGatewayDto {
  #[serde(default)]
  pub kind: ctl_ipc::GatewayKind,
  pub gateway_id: String,
  pub name: String,
  pub destination: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub hostname: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub user: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub port: Option<u16>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub identity_file: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub remote_info: Option<ctl_proto::RemoteIdentity>,
  pub mode: SshGatewayModeDto,
}

fn ssh_gateways_empty(gateways: &[SshGatewayDto]) -> bool {
  gateways.is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectionTargetDto {
  Local,
  Ssh {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_info: Option<Box<ctl_proto::RemoteIdentity>>,
    destination: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssh_config_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    use_ssh_config_master: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    gateway_route: Vec<SshGatewayRouteStepDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vpn_connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "ssh_gateways_empty")]
    gateways: Box<[SshGatewayDto]>,
  },
}

impl ConnectionTargetDto {
  /// Confirm that the SSH channel reaches the pinned account environment.
  ///
  /// # Errors
  /// Returns an error if the remote identity differs from the saved identity.
  pub fn verify_remote_identity(
    &self,
    identity: &ctl_proto::RemoteIdentity,
  ) -> Result<(), HostError> {
    if let Self::Ssh {
      remote_info: Some(expected),
      ..
    } = self
      && expected.remote_id != identity.remote_id
    {
      return Err(HostError::new(
        "remote_identity_mismatch",
        "This address now connects to a different remote environment. Add it as a separate host to keep the saved sessions intact.",
      ));
    }
    Ok(())
  }

  #[must_use]
  pub fn ssh(destination: impl Into<String>) -> Self {
    Self::Ssh {
      remote_info: None,
      destination: destination.into(),
      ssh_config_alias: None,
      use_ssh_config_master: None,
      hostname: None,
      user: None,
      port: None,
      identity_file: None,
      gateway_route: Vec::new(),
      vpn_connection_id: None,
      gateways: Box::default(),
    }
  }
}
