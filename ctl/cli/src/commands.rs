use super::{Arguments, Command, RemotePlatform};
use ctl_core::{
  ConnectionTarget, CoreError, TaskTransport, Transport, is_retryable_connection_error,
  open_task_transport_with_interaction, open_transport_with_interaction,
};
use rmux_cli::{CommandError, ConnectFuture, Connector};
use std::path::PathBuf;
use thiserror::Error;

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  let target = arguments.host.map_or_else(ConnectionTarget::local, |host| {
    ConnectionTarget::ssh_with_options(
      host,
      ctl_core::SshConnectionOptions {
        remote_platform: match arguments.remote_platform {
          Some(RemotePlatform::Windows) => ctl_core::RemotePlatform::Windows,
          Some(RemotePlatform::Unix) | None => ctl_core::RemotePlatform::Unix,
        },
        ..ctl_core::SshConnectionOptions::default()
      },
    )
  });
  let connector = CtlConnector { target };
  match arguments.command {
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
  }
  Ok(())
}

struct CtlConnector {
  target: ConnectionTarget,
}

impl CtlConnector {
  fn for_interactive_session(&self, rmux_socket: PathBuf) -> Self {
    Self {
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
      let interaction = ssh_interaction(&self.target).await?;
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
      let interaction = ssh_interaction(&self.target).await?;
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
  target: &ConnectionTarget,
) -> Result<ctl_core::SshInteraction, CtlConnectError> {
  let ConnectionTarget::Ssh {
    destination,
    options,
  } = target
  else {
    return Ok(ctl_core::SshInteraction::Inherit);
  };
  let control_path = crate::ssh_broker::ensure_master(daemon_target(destination, options)).await?;
  Ok(ctl_core::SshInteraction::Multiplexed { control_path })
}

#[cfg(unix)]
fn daemon_target(
  destination: &str,
  options: &ctl_core::SshConnectionOptions,
) -> ctld_ipc::SshTarget {
  ctld_ipc::SshTarget {
    destination: destination.to_owned(),
    ssh_config_alias: None,
    hostname: options.hostname.clone(),
    user: options.user.clone(),
    port: options.port,
    identity_file: options.identity_file.clone(),
    gateways: Vec::new(),
  }
}

#[cfg(not(unix))]
async fn ssh_interaction(
  _target: &ConnectionTarget,
) -> Result<ctl_core::SshInteraction, CtlConnectError> {
  Ok(ctl_core::SshInteraction::Inherit)
}

#[derive(Debug, Error)]
enum CtlConnectError {
  #[error(transparent)]
  Core(#[from] CoreError),
  #[cfg(unix)]
  #[error(transparent)]
  Broker(#[from] crate::ssh_broker::Error),
}

#[derive(Debug, Error)]
pub enum CliError {
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
  #[cfg(unix)]
  fn unix_broker_target_preserves_fixed_connection_options() {
    let target = ConnectionTarget::ssh_with_options(
      "work",
      ctl_core::SshConnectionOptions {
        remote_platform: ctl_core::RemotePlatform::Unix,
        hostname: Some("example.test".into()),
        user: Some("alice".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("/keys/work")),
        gateways: Vec::new(),
      },
    );
    let ConnectionTarget::Ssh {
      destination,
      options,
    } = &target
    else {
      unreachable!();
    };
    let broker = daemon_target(destination, options);

    assert_eq!(broker.destination, "work");
    assert_eq!(broker.ssh_config_alias, None);
    assert_eq!(broker.hostname.as_deref(), Some("example.test"));
    assert_eq!(broker.user.as_deref(), Some("alice"));
    assert_eq!(broker.port, Some(2222));
    assert_eq!(broker.identity_file, Some(PathBuf::from("/keys/work")));
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
    };
    let attachment = connector.for_interactive_session(PathBuf::from("/remote/rmux.sock"));
    assert_eq!(attachment.target, target);
  }

  #[test]
  fn local_interactive_sessions_use_the_backend_socket() {
    let connector = CtlConnector {
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
