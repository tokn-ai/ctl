use std::collections::HashSet;

use super::{
  ConnectionTargetDto, GatewayTailscaleBinding, HostCatalogDocument, HostError, SshGatewayDto,
  SshGatewayModeDto, SshGatewayRouteStepDto, WorkspaceConnectionMethod, WorkspaceHost,
};

pub(super) struct ResolvedRoute {
  pub gateways: Box<[SshGatewayDto]>,
  pub tailscale_bindings: Vec<GatewayTailscaleBinding>,
}

pub(super) fn resolve(
  catalog: &HostCatalogDocument,
  host: &WorkspaceHost,
  method: &WorkspaceConnectionMethod,
) -> Result<ResolvedRoute, HostError> {
  let mut resolver = RouteResolver {
    catalog,
    active: HashSet::new(),
    gateways: Vec::new(),
    tailscale_bindings: Vec::new(),
  };
  resolver.method_route(host, method)?;
  let ConnectionTargetDto::Ssh {
    vpn_connection_id, ..
  } = &method.target
  else {
    return Err(invalid_method());
  };
  // Keep the root method's legacy VPN in its original field. Nested legacy
  // VPNs become ordinary steps, whose owner follows the insertion context.
  if vpn_connection_id.is_some() {
    resolver.gateways.remove(0);
    for binding in &mut resolver.tailscale_bindings {
      binding.gateway_index -= 1;
    }
  }
  Ok(ResolvedRoute {
    gateways: resolver.gateways.into_boxed_slice(),
    tailscale_bindings: resolver.tailscale_bindings,
  })
}

struct RouteResolver<'a> {
  catalog: &'a HostCatalogDocument,
  active: HashSet<String>,
  gateways: Vec<SshGatewayDto>,
  tailscale_bindings: Vec<GatewayTailscaleBinding>,
}

impl RouteResolver<'_> {
  fn method_route(
    &mut self,
    host: &WorkspaceHost,
    method: &WorkspaceConnectionMethod,
  ) -> Result<(), HostError> {
    let key = host.host_id.clone();
    if !self.active.insert(key.clone()) {
      return Err(HostError::new(
        "host_route_cycle",
        format!(
          "The route through {} / {} contains a cycle. Edit its hops before connecting.",
          host.name, method.name
        ),
      ));
    }
    if self.active.len() > 9 {
      return Err(route_too_long());
    }
    let ConnectionTargetDto::Ssh {
      gateway_route,
      vpn_connection_id,
      ..
    } = &method.target
    else {
      return Err(invalid_method());
    };
    if let Some(connection_id) = vpn_connection_id {
      self.push(vpn_gateway(connection_id))?;
    }
    for step in gateway_route {
      self.step(step)?;
    }
    self.active.remove(&key);
    Ok(())
  }

  fn step(&mut self, step: &SshGatewayRouteStepDto) -> Result<(), HostError> {
    match step {
      SshGatewayRouteStepDto::Vpn { vpn_connection_id } => {
        self.push(vpn_gateway(vpn_connection_id))
      }
      SshGatewayRouteStepDto::Gateway { gateway_id, mode } => {
        let gateway = self
          .catalog
          .ssh_gateways
          .iter()
          .find(|item| item.gateway_id == *gateway_id)
          .ok_or_else(|| HostError::new("gateway_missing", "A saved gateway is missing."))?;
        self.push(SshGatewayDto {
          kind: gateway.kind,
          vpn_connection_id: None,
          gateway_id: gateway.gateway_id.clone(),
          name: gateway.name.clone(),
          destination: gateway.destination.clone(),
          hostname: gateway.hostname.clone(),
          user: gateway.user.clone(),
          port: gateway.port,
          identity_file: gateway.identity_file.clone(),
          remote_info: gateway.remote_info.clone(),
          mode: *mode,
        })
      }
      SshGatewayRouteStepDto::Host {
        host_id,
        method_id,
        mode,
      } => {
        let host = self
          .catalog
          .hosts
          .iter()
          .find(|host| host.host_id == *host_id)
          .ok_or_else(|| {
            HostError::new(
              "host_hop_missing",
              "A saved host used as a hop is missing. Edit the route before removing that host.",
            )
          })?;
        let method = host.connection_methods.iter().find(|method| method.method_id == *method_id)
          .ok_or_else(|| HostError::new("host_hop_method_missing", format!("The selected connection method for hop {} is missing. Edit routes that use it before removing that method.", host.name)))?;
        let ConnectionTargetDto::Ssh {
          destination,
          hostname,
          user,
          port,
          identity_file,
          ..
        } = &method.target
        else {
          return Err(invalid_method());
        };
        if identity_file.is_some() {
          return Err(HostError::new(
            "host_hop_identity_unsupported",
            format!(
              "Hop {} / {} selects an identity file that SSH hops cannot use yet. Choose a method whose key is configured in OpenSSH instead.",
              host.name, method.name
            ),
          ));
        }
        method.target.to_ssh_target()?;
        let (destination, user) = hop_endpoint(
          method.ssh_config_alias.as_ref().unwrap_or(destination),
          user.as_deref(),
        )?;
        self.method_route(host, method)?;
        if let Some(node_id) = &method.tailscale_node_id {
          self.tailscale_bindings.push(GatewayTailscaleBinding {
            gateway_index: self.gateways.len(),
            node_id: node_id.clone(),
          });
        }
        self.push(SshGatewayDto {
          kind: ctl_ipc::GatewayKind::Ssh,
          vpn_connection_id: None,
          gateway_id: format!("host:{host_id}:{method_id}"),
          name: host.name.clone(),
          destination,
          hostname: hostname.clone(),
          user,
          port: *port,
          identity_file: None,
          remote_info: host.remote_info.clone(),
          mode: *mode,
        })
      }
    }
  }

  fn push(&mut self, gateway: SshGatewayDto) -> Result<(), HostError> {
    if self.gateways.len() >= 8 {
      return Err(route_too_long());
    }
    if gateway.kind == ctl_ipc::GatewayKind::Vpn
      && self
        .gateways
        .last()
        .is_some_and(|previous| previous.kind != ctl_ipc::GatewayKind::Ssh)
    {
      return Err(HostError::new(
        "invalid_vpn_route",
        "A VPN must be first in the expanded route or follow an SSH host. Edit the route or a linked host's route.",
      ));
    }
    self.gateways.push(gateway);
    Ok(())
  }
}

fn vpn_gateway(connection_id: &str) -> SshGatewayDto {
  SshGatewayDto {
    kind: ctl_ipc::GatewayKind::Vpn,
    vpn_connection_id: Some(connection_id.into()),
    gateway_id: format!("vpn:{connection_id}"),
    name: connection_id.into(),
    destination: connection_id.into(),
    hostname: None,
    user: None,
    port: None,
    identity_file: None,
    remote_info: None,
    mode: SshGatewayModeDto::Automatic,
  }
}

fn invalid_method() -> HostError {
  HostError::new(
    "host_hop_invalid",
    "Only an SSH connection method can be used as a hop.",
  )
}

fn route_too_long() -> HostError {
  HostError::new(
    "host_route_too_long",
    "The expanded route exceeds eight hops. Shorten the route or a linked host's route.",
  )
}

fn hop_endpoint(
  destination: &str,
  user: Option<&str>,
) -> Result<(String, Option<String>), HostError> {
  let (embedded_user, destination) = destination
    .rsplit_once('@')
    .map_or((None, destination), |(user, destination)| {
      (Some(user), destination)
    });
  let user = user.or(embedded_user);
  if destination.is_empty()
    || destination.contains([',', '@'])
    || user.is_some_and(|user| user.is_empty() || user.contains([',', '@']))
  {
    return Err(HostError::new(
      "host_hop_address_unsupported",
      "A saved host used as a hop must have a valid SSH hostname and account. Edit its selected connection method.",
    ));
  }
  Ok((destination.into(), user.map(Into::into)))
}
