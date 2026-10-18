use std::ffi::OsString;
use std::process::Command;

use ctl_client::hosts::ConnectionTargetDto;

/// Keep the invocation-local askpass bridge alive while OpenSSH owns the terminal.
pub async fn caller_command(
  target: &ConnectionTargetDto,
  authentication: &crate::caller_auth::CallerAuth,
  arguments: Option<&[String]>,
  platform: ctl_client::RemotePlatform,
) -> Result<i32, Error> {
  let mut command = tokio::process::Command::new("ssh");
  authentication
    .authentication
    .configure(command.as_std_mut());
  command.args(target_arguments(&target.to_ssh_target()?)?);
  if arguments.is_some() {
    command.arg("-T");
  }
  command.arg("--").arg(target.label());
  if let Some(arguments) = arguments {
    match platform {
      ctl_client::RemotePlatform::Unix => {
        command.arg(
          arguments
            .iter()
            .map(|value| shell_quote(value))
            .collect::<Vec<_>>()
            .join(" "),
        );
      }
      ctl_client::RemotePlatform::Windows => {
        command.args(arguments);
      }
    }
  }
  command
    .stdin(std::process::Stdio::inherit())
    .stdout(std::process::Stdio::inherit())
    .stderr(std::process::Stdio::inherit())
    .kill_on_drop(true);
  let mut child = command.spawn()?;
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    loop {
      use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
      };
      let signal = tokio::select! {
        status = child.wait() => {
          use std::os::unix::process::ExitStatusExt as _;
          let status = status?;
          return Ok(status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
        },
        _ = interrupt.recv() => Signal::SIGINT,
        _ = terminate.recv() => Signal::SIGTERM,
        _ = hangup.recv() => Signal::SIGHUP,
      };
      if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        let _ = kill(Pid::from_raw(pid), signal);
      }
    }
  }
  #[cfg(not(unix))]
  Ok(child.wait().await?.code().unwrap_or(1))
}

pub async fn plain_shell(target: &ConnectionTargetDto) -> Result<i32, Error> {
  if target.is_local() {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
      if cfg!(windows) {
        "cmd.exe".into()
      } else {
        "/bin/sh".into()
      }
    });
    return run_process(Command::new(shell));
  }
  let mut command = ssh_command(target).await?;
  command.arg(target.label());
  run_process(command)
}

pub async fn execute(
  target: &ConnectionTargetDto,
  arguments: Vec<String>,
  platform: ctl_client::RemotePlatform,
) -> Result<i32, Error> {
  if target.is_local() {
    let mut command = Command::new(&arguments[0]);
    command.args(&arguments[1..]);
    return run_process(command);
  }
  let mut command = ssh_command(target).await?;
  command.arg("-T").arg(target.label());
  match platform {
    ctl_client::RemotePlatform::Unix => {
      command.arg(
        arguments
          .iter()
          .map(|arg| shell_quote(arg))
          .collect::<Vec<_>>()
          .join(" "),
      );
    }
    ctl_client::RemotePlatform::Windows => {
      command.args(arguments);
    }
  }
  run_process(command)
}

pub async fn ssh_command(target: &ConnectionTargetDto) -> Result<Command, Error> {
  crate::target::ensure_vpn(target).await?;
  let target = target.to_ssh_target()?;
  let mut command = Command::new("ssh");
  #[cfg(unix)]
  {
    let path = crate::ssh_broker::ensure_master(target.clone()).await?;
    command.args([OsString::from("-S"), path.into_os_string()]);
    command.args(["-o", "ControlMaster=no", "-o", "ProxyCommand=false"]);
    // The owner already opened this route. Reusing it needs no proxy helper
    // executable and must not install a new bundle for an existing connection.
    let mut defaults = target.clone();
    defaults.gateways.clear();
    command.args(target_arguments(&defaults)?);
  }
  #[cfg(not(unix))]
  command.args(prepared_target_arguments(&target).await?);
  Ok(command)
}

/// Explicit structured fields become defaults after any user-supplied options.
pub fn target_arguments(target: &ctl_ipc::SshTarget) -> Result<Vec<OsString>, Error> {
  target_arguments_with_proxy(target, None)
}

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
pub async fn inspected_target_arguments(
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

pub fn shell_quote(value: &str) -> String {
  format!("'{}'", value.replace('\'', "'\\''"))
}

/// Replacing the Unix process preserves terminal ownership, signals and exit
/// status, including when invoked as scp's binary SSH transport.
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
  Target(#[from] crate::target::Error),
  #[error(transparent)]
  Host(#[from] ctl_client::hosts::HostError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
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
