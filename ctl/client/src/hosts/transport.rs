use super::{ConnectionTargetDto, HostError};
use crate::{ConnectionTarget, SshConnectionOptions, SshGateway, SshGatewayMode};
use std::path::PathBuf;

/// A saved profile and the route prefix of its execution host.
pub struct VpnRoute {
  pub connection_id: String,
  pub owner: Option<ctl_ipc::SshTarget>,
  pub expected_remote_id: Option<String>,
}

impl ConnectionTargetDto {
  /// Shared route identity for broker operations and non-authenticating probes.
  ///
  /// # Errors
  /// Rejects local targets, invalid SSH addresses, and invalid VPN references.
  pub fn to_ssh_target(&self) -> Result<ctl_ipc::SshTarget, HostError> {
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
    let mut target = ctl_ipc::SshTarget {
      destination: destination.clone(),
      ssh_config_alias: ssh_config_alias.clone(),
      use_ssh_config_master: *use_ssh_config_master,
      hostname: hostname.clone(),
      user: user.clone(),
      port: *port,
      identity_file: identity_file.as_ref().map(PathBuf::from),
      gateways: self.ssh_gateways(),
    };
    if !ctl_ipc::has_valid_gateway_route(&target.gateways) {
      return Err(HostError::new(
        "invalid_vpn_route",
        "A VPN must be first in the route or follow an SSH host. Choose a valid saved VPN connection.",
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
                ctl_ipc::SshGatewayMode::Automatic => SshGatewayMode::Automatic,
                ctl_ipc::SshGatewayMode::NativeOnly => SshGatewayMode::NativeOnly,
                ctl_ipc::SshGatewayMode::AgentRelayOnly => SshGatewayMode::AgentRelayOnly,
              },
            })
            .collect(),
        },
      ),
    }
  }

  /// Stable transport settings shared by authentication, status, and cleanup.
  #[must_use]
  pub fn ssh_gateways(&self) -> Vec<ctl_ipc::SshGateway> {
    let Self::Ssh {
      vpn_connection_id,
      gateways,
      ..
    } = self
    else {
      return Vec::new();
    };
    let mut route: Vec<_> = vpn_connection_id
      .iter()
      .map(|connection_id| super::vpn_gateway(connection_id))
      .collect();
    for (index, gateway) in gateways.iter().enumerate() {
      let expected_remote_id = index
        .checked_sub(1)
        .and_then(|index| gateways.get(index))
        .filter(|owner| owner.kind == ctl_ipc::GatewayKind::Ssh)
        .and_then(|owner| owner.remote_info.as_ref())
        .map(|identity| identity.remote_id.clone());
      route.push(ctl_ipc::SshGateway {
        kind: gateway.kind,
        vpn: gateway
          .vpn_connection_id
          .as_ref()
          .map(|connection_id| ctl_ipc::VpnGateway {
            connection_id: connection_id.clone(),
            socket_path: ctl_ipc::vpn::socket_path(),
            expected_remote_id,
          }),
        destination: gateway.destination.clone(),
        hostname: gateway.hostname.clone(),
        user: gateway.user.clone(),
        port: gateway.port,
        identity_file: gateway.identity_file.as_ref().map(PathBuf::from),
        mode: match gateway.mode {
          super::SshGatewayModeDto::Automatic => ctl_ipc::SshGatewayMode::Automatic,
          super::SshGatewayModeDto::NativeOnly => ctl_ipc::SshGatewayMode::NativeOnly,
          super::SshGatewayModeDto::AgentRelayOnly => ctl_ipc::SshGatewayMode::AgentRelayOnly,
        },
      });
    }
    route
  }

  /// Collect VPNs in connection order, so each owner is reachable before startup.
  ///
  /// # Errors
  /// Rejects malformed or unsupported routes without starting any VPN.
  pub fn vpn_route(&self) -> Result<Vec<VpnRoute>, HostError> {
    if self.is_local() {
      return Ok(Vec::new());
    }
    let target = self.to_ssh_target()?;
    Ok(
      target
        .gateways
        .iter()
        .enumerate()
        .filter_map(|(index, gateway)| {
          let vpn = gateway.vpn.as_ref()?;
          let owner = ctl_ipc::vpn_owner_target(&target.gateways, index).map(|mut owner| {
            // HostName overrides must keep the original OpenSSH alias lookup.
            // The owner always uses a private master, even for an alias method.
            if owner.hostname.is_some() {
              owner.ssh_config_alias = Some(owner.destination.clone());
              owner.use_ssh_config_master = Some(false);
              owner.normalize_master_policy();
            }
            owner
          });
          Some(VpnRoute {
            connection_id: vpn.connection_id.clone(),
            owner,
            expected_remote_id: vpn.expected_remote_id.clone(),
          })
        })
        .collect(),
    )
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
