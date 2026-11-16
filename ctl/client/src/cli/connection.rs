use std::ffi::OsString;
use std::process::Command;

/// Explicit structured fields become defaults after any user-supplied options.
///
/// # Errors
/// Returns an error if the target requires a relay or a prepared proxy command.
pub fn target_arguments(target: &ctl_ipc::SshTarget) -> Result<Vec<OsString>, Error> {
  target_arguments_with_proxy(target, None)
}

/// Renders SSH arguments, preparing a ctld proxy when required.
///
/// # Errors
/// Returns an error if the target route is unsupported or proxy preparation fails.
pub async fn prepared_target_arguments(
  target: &ctl_ipc::SshTarget,
) -> Result<Vec<OsString>, Error> {
  let proxy = if target
    .gateways
    .iter()
    .any(ctl_ipc::SshGateway::requires_proxy_command)
  {
    Some(ctl_ipc::prepare_proxy_command(&target.gateways).await?)
  } else {
    None
  };
  target_arguments_with_proxy(target, proxy)
}

/// Render SSH configuration using the selected helper without preparing it.
pub(crate) async fn inspected_target_arguments(
  target: &ctl_ipc::SshTarget,
) -> Result<Vec<OsString>, Error> {
  let proxy = if target
    .gateways
    .iter()
    .any(ctl_ipc::SshGateway::requires_proxy_command)
  {
    let selection = ctl_ipc::resolver::resolve(ctl_ipc::resolver::Request::new(
      ctl_ipc::resolver::Intent::Inspect,
    ))
    .await?;
    // OpenSSH -G prints this command without executing it. A selected package
    // awaiting preparation must retain its identity and remain uninstalled.
    Some(ctl_ipc::proxy_command_with_executable(
      &target.gateways,
      &selection.executable,
    ))
  } else {
    None
  };
  target_arguments_with_proxy(target, proxy)
}

fn target_arguments_with_proxy(
  target: &ctl_ipc::SshTarget,
  proxy: Option<String>,
) -> Result<Vec<OsString>, Error> {
  let mut result = Vec::new();
  let mut option = |value: String| {
    result.push("-o".into());
    result.push(value.into());
  };
  if let Some(host) = &target.hostname {
    option(format!("Hostname={host}"));
  }
  if let Some(user) = &target.user {
    option(format!("User={user}"));
  }
  if let Some(port) = target.port {
    option(format!("Port={port}"));
  }
  if target
    .gateways
    .iter()
    .any(|gateway| gateway.mode == ctl_ipc::SshGatewayMode::AgentRelayOnly)
  {
    return Err(Error::AgentRelay);
  }
  if target
    .gateways
    .iter()
    .any(ctl_ipc::SshGateway::requires_proxy_command)
  {
    let proxy = proxy.ok_or_else(|| {
      Error::Arguments(
        "A proxy route requires an inspected or prepared ctld helper selection.".into(),
      )
    })?;
    option(format!("ProxyCommand={proxy}"));
    // Never let a direct SSH-config master bypass the selected route.
    option("ControlPath=none".into());
  } else if !target.gateways.is_empty() {
    option(format!(
      "ProxyJump={}",
      target
        .gateways
        .iter()
        .map(|gateway| {
          let host = gateway.hostname.as_deref().unwrap_or(&gateway.destination);
          let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
          } else {
            host.into()
          };
          format!(
            "{}{}{}",
            gateway
              .user
              .as_ref()
              .map_or(String::new(), |user| format!("{user}@")),
            host,
            gateway
              .port
              .map_or(String::new(), |port| format!(":{port}"))
          )
        })
        .collect::<Vec<_>>()
        .join(",")
    ));
  }
  if let Some(identity) = &target.identity_file {
    result.extend([OsString::from("-i"), identity.as_os_str().to_owned()]);
  }
  Ok(result)
}

#[must_use]
pub fn shell_quote(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

/// Replacing the Unix process preserves terminal ownership, signals and exit
/// status, including when invoked as scp's binary SSH transport.
///
/// # Errors
/// Returns an error if the command cannot be launched or waited on.
pub fn run_process(mut command: Command) -> Result<i32, Error> {
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt as _;
    Err(Error::Process(command.exec()))
  }
  #[cfg(not(unix))]
  {
    Ok(command.status()?.code().unwrap_or(1))
  }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(transparent)]
  Target(#[from] super::target::Error),
  #[error(transparent)]
  Host(#[from] crate::hosts::HostError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] super::ssh_broker::Error),
  #[error(transparent)]
  Connect(#[from] ctl_ipc::ConnectError),
  #[error("Could not launch command: {0}")]
  Process(#[from] std::io::Error),
  #[error(
    "This route requires an agent relay; select a connection method that supports OpenSSH forwarding."
  )]
  AgentRelay,
  #[error("{0}")]
  Arguments(String),
}
