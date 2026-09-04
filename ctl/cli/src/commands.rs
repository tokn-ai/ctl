use super::{Arguments, Command, RemotePlatform};
use ctl_core::{
  ConnectionTarget, CoreError, TaskTransport, Transport, is_retryable_connection_error,
  open_task_transport, open_transport,
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
  type Error = CoreError;

  fn connect_task(&self) -> task_cli::ConnectFuture<'_, TaskTransport, CoreError> {
    Box::pin(open_task_transport(&self.target))
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
  type Error = CoreError;

  fn connect(&self) -> ConnectFuture<'_, Transport, CoreError> {
    Box::pin(open_transport(&self.target))
  }

  fn is_retryable(&self, error: &CoreError) -> bool {
    is_retryable_connection_error(error)
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
  fn remote_interactive_sessions_keep_the_task_host_and_connection_options() {
    let target = ConnectionTarget::ssh_with_options(
      "task-server",
      ctl_core::SshConnectionOptions {
        remote_platform: ctl_core::RemotePlatform::Windows,
        hostname: Some("server.example".into()),
        user: Some("task-user".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("task-key")),
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
