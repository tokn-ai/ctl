use super::{Arguments, Command};
use ctl_core::{
  ConnectionTarget, CoreError, Transport, is_retryable_connection_error, open_transport,
};
use rmux_cli::{CommandError, ConnectFuture, Connector};
use thiserror::Error;

pub async fn run(arguments: Arguments) -> Result<(), CliError> {
  let target = arguments
    .host
    .map_or_else(ConnectionTarget::local, ConnectionTarget::ssh);
  let connector = CtlConnector { target };
  match arguments.command {
    Command::Rmux { command } => rmux_cli::run(command, &connector).await?,
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
}
