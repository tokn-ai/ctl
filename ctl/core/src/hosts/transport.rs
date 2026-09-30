use super::{ConnectionTargetDto, HostError};
use crate::{ConnectionTarget, SshConnectionOptions, SshGateway, SshGatewayMode};
use std::path::PathBuf;

impl ConnectionTargetDto {
  /// Shared route identity for broker operations and non-authenticating probes.
  ///
  /// # Errors
  /// Rejects local targets, invalid SSH addresses, and invalid VPN references.
  pub fn to_ssh_target(&self) -> Result<ctld_ipc::SshTarget, HostError> {
    let Self::Ssh {
      destination,
      ssh_config_alias,
      use_ssh_config_master,
      hostname,
      user,
      port,
      identity_file,
      ..
    } = self
    else {
      return Err(HostError::new(
        "invalid_ssh_target",
        "Select a remote SSH host.",
      ));
    };
    if destination.is_empty()
      || destination.starts_with('-')
      || destination
        .chars()
        .any(|value| value.is_control() || value.is_whitespace())
      || [hostname, user].into_iter().flatten().any(|value| {
        value.is_empty()
          || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
      })
    {
      return Err(HostError::new(
        "invalid_ssh_target",
        "The saved SSH address or account is invalid.",
      ));
    }
    let mut target = ctld_ipc::SshTarget {
      destination: destination.clone(),
      ssh_config_alias: ssh_config_alias.clone(),
      use_ssh_config_master: *use_ssh_config_master,
      hostname: hostname.clone(),
      user: user.clone(),
      port: *port,
      identity_file: identity_file.as_ref().map(PathBuf::from),
      gateways: self.ssh_gateways(),
    };
    if target
      .gateways
      .iter()
      .any(|gateway| !gateway.has_valid_vpn_configuration())
    {
      return Err(HostError::new(
        "invalid_vpn_route",
        "Choose a saved VPN connection in the host settings.",
      ));
    }
    target.normalize_master_policy();
    Ok(target)
  }

  #[must_use]
  pub fn to_core(&self) -> ConnectionTarget {
    match self {
      Self::Local => ConnectionTarget::local(),
      Self::Ssh {
        destination,
        hostname,
        user,
        port,
        identity_file,
        ..
      } => ConnectionTarget::ssh_with_options(
        destination.clone(),
        SshConnectionOptions {
          remote_platform: crate::RemotePlatform::Unix,
          hostname: hostname.clone(),
          user: user.clone(),
          port: *port,
          identity_file: identity_file.as_ref().map(PathBuf::from),
          gateways: self
            .ssh_gateways()
            .into_iter()
            .map(|gateway| SshGateway {
              kind: gateway.kind,
              vpn: gateway.vpn,
              destination: gateway.destination,
              hostname: gateway.hostname,
              user: gateway.user,
              port: gateway.port,
              identity_file: gateway.identity_file,
              mode: match gateway.mode {
                ctld_ipc::SshGatewayMode::Automatic => SshGatewayMode::Automatic,
                ctld_ipc::SshGatewayMode::NativeOnly => SshGatewayMode::NativeOnly,
                ctld_ipc::SshGatewayMode::AgentRelayOnly => SshGatewayMode::AgentRelayOnly,
              },
            })
            .collect(),
        },
      ),
    }
  }

  /// Stable transport settings shared by authentication, status, and cleanup.
  #[must_use]
  pub fn ssh_gateways(&self) -> Vec<ctld_ipc::SshGateway> {
    let Self::Ssh {
      vpn_connection_id,
      gateways,
      ..
    } = self
    else {
      return Vec::new();
    };
    vpn_connection_id
      .iter()
      .map(|connection_id| super::vpn_gateway(connection_id))
      .chain(gateways.iter().map(|gateway| ctld_ipc::SshGateway {
        kind: gateway.kind,
        vpn: None,
        destination: gateway.destination.clone(),
        hostname: gateway.hostname.clone(),
        user: gateway.user.clone(),
        port: gateway.port,
        identity_file: gateway.identity_file.as_ref().map(PathBuf::from),
        mode: match gateway.mode {
          super::SshGatewayModeDto::Automatic => ctld_ipc::SshGatewayMode::Automatic,
          super::SshGatewayModeDto::NativeOnly => ctld_ipc::SshGatewayMode::NativeOnly,
          super::SshGatewayModeDto::AgentRelayOnly => ctld_ipc::SshGatewayMode::AgentRelayOnly,
        },
      }))
      .collect()
  }

  #[must_use]
  pub fn is_local(&self) -> bool {
    matches!(self, Self::Local)
  }

  #[must_use]
  pub fn label(&self) -> &str {
    match self {
      Self::Local => "local",
      Self::Ssh { destination, .. } => destination,
    }
  }
}
