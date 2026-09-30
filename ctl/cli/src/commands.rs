use super::{Arguments, Command, RemotePlatform};
use ctl_core::{
  ConnectionTarget, CoreError, TaskTransport, Transport, is_retryable_connection_error,
  open_task_transport_with_interaction, open_transport_with_interaction,
};
use rmux_cli::{CommandError, ConnectFuture, Connector};
use std::path::PathBuf;
use thiserror::Error;

pub async fn run(arguments: Arguments) -> Result<i32, CliError> {
  if arguments.host.is_some() {
    if matches!(arguments.command, Command::Vpn { .. }) {
      return Err(CliError::RemoteVpnUnsupported);
    }
    if matches!(arguments.command, Command::Taskd { .. }) {
      return Err(CliError::RemoteTaskDaemonRestartUnsupported);
    }
    if matches!(arguments.command, Command::Ssh { .. } | Command::Scp { .. }) {
      return Err(CliError::CompatibilityHost);
    }
  }
  match arguments.command {
    Command::Ssh {
      arguments: ssh_arguments,
    } => return Ok(crate::openssh::run_ssh(ssh_arguments, arguments.method.as_deref()).await?),
    Command::Scp {
      arguments: scp_arguments,
    } => {
      return Ok(crate::openssh::run_scp(
        scp_arguments,
        arguments.method.as_deref(),
      )?);
    }
    _ => {}
  }
  let resolved =
    crate::target::resolve(arguments.host.as_deref(), arguments.method.as_deref()).await?;
  let platform = match arguments.remote_platform {
    Some(RemotePlatform::Windows) => ctl_core::RemotePlatform::Windows,
    _ => ctl_core::RemotePlatform::Unix,
  };
  let mut target = resolved.target.to_core();
  if let ConnectionTarget::Ssh { options, .. } = &mut target {
    options.remote_platform = platform;
  }
  let connector = CtlConnector {
    target,
    settings: resolved.target,
  };
  match arguments.command {
    Command::Shell {
      session,
      plain,
      cwd,
    } => return run_shell(&connector, session, plain, cwd).await,
    Command::Exec { command } => {
      return Ok(crate::connection::execute(&connector.settings, command, platform).await?);
    }
    #[cfg(unix)]
    Command::Port { command } => crate::port::run(&connector.settings, command).await?,
    Command::Ssh { .. } | Command::Scp { .. } => {
      unreachable!("compatibility commands dispatched above")
    }
    Command::Rmux { command } => {
      rmux_cli::run(command, &connector).await?;
    }
    Command::Taskd {
      command: super::TaskdCommand::Restart,
    } => {
      if !connector.target.is_local() {
        return Err(CliError::RemoteTaskDaemonRestartUnsupported);
      }
      task_client::restart_daemon().await?;
      println!("taskd is ready");
    }
    Command::Task { command } => {
      task_cli::run_with_connector(command, &connector).await?;
    }
    Command::Vpn { command } => {
      if !connector.target.is_local() {
        return Err(CliError::RemoteVpnUnsupported);
      }
      crate::vpn::run(command).await?;
    }
  }
  Ok(0)
}

async fn run_shell(
  connector: &CtlConnector,
  session: Option<String>,
  plain: bool,
  cwd: Option<String>,
) -> Result<i32, CliError> {
  use std::io::IsTerminal as _;
  if plain {
    return Ok(crate::connection::plain_shell(&connector.settings).await?);
  }
  if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
    return Err(CliError::TerminalRequired);
  }
  let attach_if_exists = session.is_some();
  let session =
    rmux_cli::new_session(connector, session, Vec::new(), cwd, attach_if_exists).await?;
  rmux_cli::run(
    rmux_cli::Command::Attach {
      session: Some(session.session_id),
      target: None,
      raw: true,
      resume_from: None,
      read_only: false,
      resize: false,
    },
    connector,
  )
  .await?;
  Ok(0)
}

struct CtlConnector {
  target: ConnectionTarget,
  settings: ctl_core::hosts::ConnectionTargetDto,
}

impl CtlConnector {
  async fn identified_remote(
    &self,
    interaction: &ctl_core::SshInteraction,
    service: ctl_core::RemoteService,
  ) -> Result<Option<ctl_core::SshTransport>, CtlConnectError> {
    let ctl_core::hosts::ConnectionTargetDto::Ssh {
      remote_info: Some(_),
      ..
    } = &self.settings
    else {
      return Ok(None);
    };
    let ConnectionTarget::Ssh {
      destination,
      options,
    } = &self.target
    else {
      return Ok(None);
    };
    let stream =
      ctl_core::open_identified_ssh_service(destination, options, interaction, service).await?;
    let identity = stream.remote_identity.as_ref().ok_or_else(|| {
      ctl_core::hosts::HostError::new(
        "identity_missing",
        "The remote host did not provide its saved identity.",
      )
    })?;
    self.settings.verify_remote_identity(identity)?;
    Ok(Some(stream))
  }

  fn for_interactive_session(&self, rmux_socket: PathBuf) -> Self {
    Self {
      settings: self.settings.clone(),
      target: match &self.target {
        ConnectionTarget::Local { .. } => ConnectionTarget::Local {
          socket_path: rmux_socket,
        },
        ConnectionTarget::Ssh { .. } => self.target.clone(),
      },
    }
  }
}

impl task_cli::Connector for CtlConnector {
  type Stream = TaskTransport;
  type Error = CtlConnectError;

  fn connect_task(&self) -> task_cli::ConnectFuture<'_, TaskTransport, CtlConnectError> {
    Box::pin(async {
      let interaction = ssh_interaction(&self.settings).await?;
      if let Some(stream) = self
        .identified_remote(&interaction, ctl_core::RemoteService::Task)
        .await?
      {
        return Ok(Transport::Ssh(stream));
      }
      open_task_transport_with_interaction(&self.target, &interaction)
        .await
        .map_err(Into::into)
    })
  }

  fn is_local_task_target(&self) -> bool {
    self.target.is_local()
  }

  fn attach_interactive(
    &self,
    session: String,
    rmux_socket: PathBuf,
  ) -> task_cli::AttachFuture<'_> {
    let connector = self.for_interactive_session(rmux_socket);
    Box::pin(async move { task_cli::attach_session(session, &connector).await })
  }
}

impl Connector for CtlConnector {
  type Stream = Transport;
  type Error = CtlConnectError;

  fn connect(&self) -> ConnectFuture<'_, Transport, CtlConnectError> {
    Box::pin(async {
      let interaction = ssh_interaction(&self.settings).await?;
      if let Some(stream) = self
        .identified_remote(&interaction, ctl_core::RemoteService::Rmux)
        .await?
      {
        return Ok(Transport::Ssh(stream));
      }
      open_transport_with_interaction(&self.target, &interaction)
        .await
        .map_err(Into::into)
    })
  }

  fn is_retryable(&self, error: &CtlConnectError) -> bool {
    matches!(error, CtlConnectError::Core(error) if is_retryable_connection_error(error))
  }

  fn is_local(&self) -> bool {
    self.target.is_local()
  }

  fn label(&self) -> &str {
    self.target.label()
  }

  fn connection_kind(&self) -> &'static str {
    if self.target.is_local() {
      "local"
    } else {
      "SSH"
    }
  }

  fn client_name(&self) -> &'static str {
    "ctl"
  }

  fn status_prefix(&self) -> &'static str {
    "ctl"
  }
}

#[cfg(unix)]
async fn ssh_interaction(
  target: &ctl_core::hosts::ConnectionTargetDto,
) -> Result<ctl_core::SshInteraction, CtlConnectError> {
  if target.is_local() {
    return Ok(ctl_core::SshInteraction::Inherit);
  }
  crate::target::ensure_vpn(target).await?;
  let control_path = crate::ssh_broker::ensure_master(target.to_ssh_target()?).await?;
  Ok(ctl_core::SshInteraction::Multiplexed { control_path })
}

#[cfg(not(unix))]
async fn ssh_interaction(
  _target: &ctl_core::hosts::ConnectionTargetDto,
) -> Result<ctl_core::SshInteraction, CtlConnectError> {
  Ok(ctl_core::SshInteraction::Inherit)
}

#[derive(Debug, Error)]
enum CtlConnectError {
  #[error(transparent)]
  Host(#[from] ctl_core::hosts::HostError),
  #[error(transparent)]
  Target(#[from] crate::target::Error),
  #[error(transparent)]
  Core(#[from] CoreError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
}

#[derive(Debug, Error)]
pub enum CliError {
  #[error(transparent)]
  Host(#[from] ctl_core::hosts::HostError),
  #[error(transparent)]
  Connection(#[from] crate::connection::Error),
  #[cfg(unix)]
  #[error(transparent)]
  Port(#[from] crate::port::Error),
  #[error("Use the destination argument with ssh/scp, rather than --host.")]
  CompatibilityHost,
  #[error("ctl shell requires a terminal; use ctl exec for commands or ctl rmux new --detached.")]
  TerminalRequired,
  #[error("VPN management is only supported locally; omit --host")]
  RemoteVpnUnsupported,
  #[error(transparent)]
  Vpn(#[from] crate::vpn::Error),
  #[error("taskd restart is only supported locally; run it on the task host")]
  RemoteTaskDaemonRestartUnsupported,
  #[error(transparent)]
  Rmux(#[from] CommandError),
  #[error(transparent)]
  TaskDaemon(#[from] task_client::ClientError),
  #[error(transparent)]
  Task(#[from] task_cli::CommandError),
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn remote_vpn_commands_are_rejected_before_connecting() {
    use clap::Parser;

    for action in ["start", "status", "stop"] {
      let arguments =
        Arguments::try_parse_from(["ctl", "--host", "vpn-server", "vpn", action]).unwrap();
      assert!(matches!(
        run(arguments).await,
        Err(CliError::RemoteVpnUnsupported)
      ));
    }
  }

  #[tokio::test]
  async fn remote_daemon_restart_is_rejected_before_connecting() {
    use clap::Parser;
    let arguments =
      Arguments::try_parse_from(["ctl", "--host", "task-server", "taskd", "restart"]).unwrap();
    assert!(matches!(
      run(arguments).await,
      Err(CliError::RemoteTaskDaemonRestartUnsupported)
    ));
  }

  #[test]
  fn remote_interactive_sessions_keep_the_task_host_and_connection_options() {
    let target = ConnectionTarget::ssh_with_options(
      "task-server",
      ctl_core::SshConnectionOptions {
        remote_platform: ctl_core::RemotePlatform::Windows,
        hostname: Some("server.example".into()),
        user: Some("task-user".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("task-key")),
        gateways: Vec::new(),
      },
    );
    let connector = CtlConnector {
      target: target.clone(),
      settings: ctl_core::hosts::ConnectionTargetDto::ssh("task-server"),
    };
    let attachment = connector.for_interactive_session(PathBuf::from("/remote/rmux.sock"));
    assert_eq!(attachment.target, target);
  }

  #[test]
  fn local_interactive_sessions_use_the_backend_socket() {
    let connector = CtlConnector {
      settings: ctl_core::hosts::ConnectionTargetDto::Local,
      target: ConnectionTarget::Local {
        socket_path: PathBuf::from("/default/rmux.sock"),
      },
    };
    let attachment = connector.for_interactive_session(PathBuf::from("/backend/rmux.sock"));
    assert_eq!(
      attachment.target,
      ConnectionTarget::Local {
        socket_path: PathBuf::from("/backend/rmux.sock"),
      }
    );
  }
}
