use ctl_client::hosts::{
  ConnectionTargetDto, HostCatalogDocument, SshGatewayModeDto, SshGatewayRouteStepDto,
  WorkspaceConnectionMethod, WorkspaceHost,
};

use super::{Command, Error, MethodCommand, host_index, method_index, reject_method};

#[derive(Debug, Default, clap::Args)]
pub struct ConnectionOptions {
  #[arg(long)]
  hostname: Option<String>,
  #[arg(long)]
  user: Option<String>,
  #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
  port: Option<u16>,
  #[arg(long)]
  identity_file: Option<String>,
  /// Treat the destination as an SSH config alias.
  #[arg(long)]
  ssh_config: bool,
  /// Opt in/out of the SSH config alias's own `ControlMaster`.
  #[arg(long, value_name = "BOOL")]
  use_ssh_config_master: Option<bool>,
  /// Saved VPN connection ID.
  #[arg(long)]
  vpn: Option<String>,
  /// Saved gateway ID, repeat in route order (automatic mode).
  #[arg(long)]
  gateway: Vec<String>,
  #[arg(long)]
  tailscale_node_id: Option<String>,
  /// Remove optional settings instead of replacing them; comma-separated.
  #[arg(long, value_enum, value_delimiter = ',')]
  clear: Vec<Clear>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Clear {
  Hostname,
  User,
  Port,
  IdentityFile,
  SshConfig,
  SshConfigMaster,
  Vpn,
  Gateways,
  Tailscale,
}

impl ConnectionOptions {
  fn apply(
    &self,
    method: &mut WorkspaceConnectionMethod,
    destination: Option<String>,
  ) -> Result<(), Error> {
    let ConnectionTargetDto::Ssh {
      destination: address,
      hostname,
      user,
      port,
      identity_file,
      vpn_connection_id,
      gateway_route,
      ..
    } = &mut method.target
    else {
      return Err(Error::Usage(
        "Only SSH connection methods can be edited.".into(),
      ));
    };
    for (clear, supplied) in [
      (Clear::Hostname, self.hostname.is_some()),
      (Clear::User, self.user.is_some()),
      (Clear::Port, self.port.is_some()),
      (Clear::IdentityFile, self.identity_file.is_some()),
      (Clear::Vpn, self.vpn.is_some()),
      (Clear::Gateways, !self.gateway.is_empty()),
      (Clear::SshConfig, self.ssh_config),
      (Clear::SshConfigMaster, self.use_ssh_config_master.is_some()),
      (Clear::Tailscale, self.tailscale_node_id.is_some()),
    ] {
      if supplied && self.clear.contains(&clear) {
        return Err(Error::Usage(format!(
          "Cannot set and clear {clear:?} in the same operation."
        )));
      }
    }
    if let Some(destination) = destination {
      *address = destination;
    }
    patch(
      hostname,
      self.hostname.as_ref(),
      self.clear.contains(&Clear::Hostname),
    );
    patch(user, self.user.as_ref(), self.clear.contains(&Clear::User));
    patch(port, self.port.as_ref(), self.clear.contains(&Clear::Port));
    patch(
      identity_file,
      self.identity_file.as_ref(),
      self.clear.contains(&Clear::IdentityFile),
    );
    patch(
      vpn_connection_id,
      self.vpn.as_ref(),
      self.clear.contains(&Clear::Vpn),
    );
    patch(
      &mut method.tailscale_node_id,
      self.tailscale_node_id.as_ref(),
      self.clear.contains(&Clear::Tailscale),
    );
    patch(
      &mut method.use_ssh_config_master,
      self.use_ssh_config_master.as_ref(),
      self.clear.contains(&Clear::SshConfigMaster),
    );
    if self.clear.contains(&Clear::SshConfig) {
      method.ssh_config_alias = None;
    } else if self.ssh_config || method.ssh_config_alias.is_some() {
      method.ssh_config_alias = Some(address.clone());
    }
    if self.clear.contains(&Clear::Gateways) {
      gateway_route.clear();
    }
    if !self.gateway.is_empty() {
      *gateway_route = self
        .gateway
        .iter()
        .map(|id| SshGatewayRouteStepDto {
          gateway_id: id.clone(),
          mode: SshGatewayModeDto::Automatic,
        })
        .collect();
    }
    // Apply the same transport validation used before connecting, while all
    // changes still exist only in memory. Catalog validation checks route IDs.
    method.target.to_ssh_target()?;
    Ok(())
  }
}

fn patch<T: Clone>(field: &mut Option<T>, value: Option<&T>, clear: bool) {
  if clear {
    *field = None;
  } else if let Some(value) = value {
    *field = Some(value.clone());
  }
}

fn new_method(
  name: String,
  destination: String,
  options: &ConnectionOptions,
) -> Result<WorkspaceConnectionMethod, Error> {
  let mut method = WorkspaceConnectionMethod {
    method_id: uuid::Uuid::new_v4().to_string(),
    name,
    ssh_config_alias: None,
    use_ssh_config_master: None,
    tailscale_node_id: None,
    target: ConnectionTargetDto::ssh(destination),
  };
  options.apply(&mut method, None)?;
  Ok(method)
}

fn unique_name<'a>(
  name: &str,
  others: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<(), Error> {
  if name == "local"
    || others
      .into_iter()
      .any(|(id, other)| name == id || name == other)
  {
    return Err(Error::Usage(format!(
      "The name {name:?} is already in use or reserved. Choose a distinct name."
    )));
  }
  Ok(())
}

pub(super) fn apply(
  document: &mut HostCatalogDocument,
  command: Command,
  selected: Option<&str>,
) -> Result<(WorkspaceHost, bool), Error> {
  let result = match command {
    Command::Add {
      name,
      destination,
      method_name,
      options,
      json,
    } => {
      reject_method(selected)?;
      unique_name(
        &name,
        document
          .hosts
          .iter()
          .map(|host| (host.host_id.as_str(), host.name.as_str())),
      )?;
      let method = new_method(method_name, destination, &options)?;
      let host = WorkspaceHost {
        host_id: uuid::Uuid::new_v4().to_string(),
        name,
        preferred_method_id: Some(method.method_id.clone()),
        connection_methods: vec![method],
        remote_info: None,
      };
      document.hosts.push(host.clone());
      (host, json)
    }
    Command::Update {
      host,
      name,
      destination,
      options,
      json,
    } => {
      let index = host_index(document, &host)?;
      if let Some(name) = name {
        unique_name(
          &name,
          document
            .hosts
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, host)| (host.host_id.as_str(), host.name.as_str())),
        )?;
        document.hosts[index].name = name;
      }
      let host = &mut document.hosts[index];
      let index = method_index(host, selected)?;
      options.apply(&mut host.connection_methods[index], destination)?;
      (host.clone(), json)
    }
    Command::Remove { host, json } => {
      reject_method(selected)?;
      let index = host_index(document, &host)?;
      (document.hosts.remove(index), json)
    }
    Command::Method { command } => {
      reject_method(selected)?;
      edit_method(document, command)?
    }
    _ => return Err(Error::Usage("Expected a host editing command.".into())),
  };
  document.validate()?;
  Ok(result)
}

fn edit_method(
  document: &mut HostCatalogDocument,
  command: MethodCommand,
) -> Result<(WorkspaceHost, bool), Error> {
  let selector = match &command {
    MethodCommand::Add { host, .. }
    | MethodCommand::Update { host, .. }
    | MethodCommand::Remove { host, .. }
    | MethodCommand::Prefer { host, .. } => host,
  };
  let index = host_index(document, selector)?;
  let host = &mut document.hosts[index];
  let json = match command {
    MethodCommand::Add {
      name,
      destination,
      prefer,
      options,
      json,
      ..
    } => {
      unique_name(
        &name,
        host
          .connection_methods
          .iter()
          .map(|method| (method.method_id.as_str(), method.name.as_str())),
      )?;
      let method = new_method(name, destination, &options)?;
      if prefer {
        host.preferred_method_id = Some(method.method_id.clone());
      }
      host.connection_methods.push(method);
      json
    }
    MethodCommand::Update {
      method_name,
      name,
      destination,
      options,
      json,
      ..
    } => {
      let index = method_index(host, Some(&method_name))?;
      if let Some(name) = name {
        unique_name(
          &name,
          host
            .connection_methods
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, method)| (method.method_id.as_str(), method.name.as_str())),
        )?;
        host.connection_methods[index].name = name;
      }
      options.apply(&mut host.connection_methods[index], destination)?;
      json
    }
    MethodCommand::Remove {
      method_name, json, ..
    } => {
      let index = method_index(host, Some(&method_name))?;
      if host.preferred_method_id.as_deref()
        == Some(host.connection_methods[index].method_id.as_str())
      {
        return Err(Error::Usage("Choose another preferred method before removing this one. Use host remove to remove the entire host.".into()));
      }
      host.connection_methods.remove(index);
      json
    }
    MethodCommand::Prefer {
      method_name, json, ..
    } => {
      let index = method_index(host, Some(&method_name))?;
      host.preferred_method_id = Some(host.connection_methods[index].method_id.clone());
      json
    }
  };
  Ok((host.clone(), json))
}
