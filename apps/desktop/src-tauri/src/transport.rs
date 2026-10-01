#[cfg(test)]
use ctl_client::{ConnectionTarget, SshConnectionOptions};
use ctl_client::{Transport, open_transport};
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::timeout;

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};
use crate::local_transport;

const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn connect(target: &ConnectionTargetDto) -> CommandResult<Transport> {
  if !target.is_local() {
    return crate::ssh_auth::connect(target).await;
  }
  timeout(CONNECTION_TIMEOUT, open_transport(&target.to_core()))
    .await
    .map_err(|_elapsed| {
      CommandErrorDto::new(
        "connection_timeout",
        format!(
          "{} did not establish an ctmux connection within ten seconds",
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn app_local_settings_map_to_structured_core_options() {
    let target = ConnectionTargetDto::Ssh {
      ssh_config_alias: None,
      use_ssh_config_master: None,
      remote_info: None,
      destination: "ctmux-remote-test".into(),
      hostname: Some("127.0.0.1".into()),
      user: Some("ctmux".into()),
      port: Some(2222),
      identity_file: Some("~/.ssh/local.id_rsa".into()),
      gateway_route: Vec::new(),
      vpn_connection_id: None,
      gateways: Box::default(),
    };

    assert_eq!(
      target.to_core(),
      ConnectionTarget::Ssh {
        destination: "ctmux-remote-test".into(),
        options: SshConnectionOptions {
          remote_platform: ctl_client::RemotePlatform::Unix,
          hostname: Some("127.0.0.1".into()),
          user: Some("ctmux".into()),
          port: Some(2222),
          identity_file: Some(PathBuf::from("~/.ssh/local.id_rsa")),
          gateways: Vec::new(),
        },
      }
    );
  }
}
