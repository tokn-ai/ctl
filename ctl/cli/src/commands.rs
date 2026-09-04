use super::{Arguments, Command, RemotePlatform};
use ctl_core::{
  ConnectionTarget, CoreError, Transport, is_retryable_connection_error, open_transport,
};
use rmux_cli::{CommandError, ConnectFuture, Connector};
use thiserror::Error;

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  match arguments.command {
    Command::Rmux { command } => {
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
      rmux_cli::run(command, &CtlConnector { target }).await?;
    }
    Command::Taskd {
      command: super::TaskdCommand::Restart,
    } => {
      if let Some(host) = arguments.host {
        return Err(CliError::RemoteTaskUnsupported(host));
      }
      task_client::restart_daemon().await?;
      println!("taskd is ready");
    }
    Command::Task { command } => {
      if let Some(host) = arguments.host {
        return Err(CliError::RemoteTaskUnsupported(host));
      }
      task_cli::run(command).await?;
    }
  }
  Ok(())
}

struct CtlConnector {
  target: ConnectionTarget,
}

impl Connector for CtlConnector {
  type Stream = Transport;
  type Error = CoreError;

  fn connect(&self) -> ConnectFuture<'_, Self::Stream, Self::Error> {
    Box::pin(open_transport(&self.target))
  }

  fn is_retryable(&self, error: &Self::Error) -> bool {
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
  #[error(transparent)]
  Rmux(#[from] CommandError),
  #[error(transparent)]
  TaskDaemon(#[from] task_client::ClientError),
  #[error(transparent)]
  Task(#[from] task_cli::CommandError),
  #[error("remote task routing is not implemented yet for host {0:?}")]
  RemoteTaskUnsupported(String),
}
