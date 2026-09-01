use ctl_core::{ConnectionTarget, Transport, open_transport};
use std::time::Duration;
use tokio::time::timeout;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use crate::local_transport;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn connect(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  timeout(CONNECTION_TIMEOUT, open_transport(&target.to_core()))
    .await
    .map_err(|_elapsed| {
      CommandErrorDto::new(
        "connection_timeout",
        format!(
          "{} did not establish an rmux connection within ten seconds",
          target.label()
        ),
      )
    })?
    .map_err(|error| CommandErrorDto::transport(&error))
}

/// Opens a supplemental connection without replacing a vanished local daemon.
///
/// Session-list metadata inspection is best effort. If the local daemon exits
/// after the authoritative list response, starting a new daemon here would
/// return metadata from a different session owner. Remote SSH channels cannot
/// distinguish that race and therefore use the ordinary fixed transport.
pub async fn connect_existing(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  match target {
    ConnectionTargetDto::Local => {
      #[cfg(unix)]
      {
        Ok(Transport::Local(local_transport::connect_existing().await?))
      }
      #[cfg(not(unix))]
      {
        connect(target).await
      }
    }
    ConnectionTargetDto::Ssh { .. } => connect(target).await,
  }
}

impl ConnectionTargetDto {
  #[must_use]
  pub fn to_core(&self) -> ConnectionTarget {
    match self {
      Self::Local => ConnectionTarget::local(),
      Self::Ssh { destination } => ConnectionTarget::ssh(destination.clone()),
    }
  }

  #[must_use]
  pub fn is_local(&self) -> bool {
    matches!(self, Self::Local)
  }

  #[must_use]
  pub fn label(&self) -> &str {
    match self {
      Self::Local => "local",
      Self::Ssh { destination } => destination,
    }
  }
}
