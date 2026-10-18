//! Managed local forwarding is independent of control-master availability.

use ctl_client::connection::{self, Error};
use ctl_ipc::{ClientMessage, LocalPortForward, PortForwardStatus, ServerMessage, SshTarget};

use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

pub(super) async fn configure_port_forward(
  target: &ConnectionTargetDto,
  forward: LocalPortForward,
  enabled: bool,
) -> CommandResult<PortForwardStatus> {
  match request(ClientMessage::ConfigurePortForward {
    target: forwarding_target(target)?,
    forward,
    enabled,
  })
  .await?
  {
    ServerMessage::PortForwardConfigured { status } => Ok(status),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected port-forward response.",
    )),
  }
}

pub(super) async fn list_port_forwards(
  target: &ConnectionTargetDto,
) -> CommandResult<Vec<PortForwardStatus>> {
  match request(ClientMessage::ListPortForwards {
    target: forwarding_target(target)?,
  })
  .await?
  {
    ServerMessage::PortForwards { statuses } => Ok(statuses),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected port-forward list.",
    )),
  }
}

fn forwarding_target(target: &ConnectionTargetDto) -> CommandResult<SshTarget> {
  #[cfg(not(unix))]
  if matches!(
    target,
    ConnectionTargetDto::Ssh {
      use_ssh_config_master: Some(true),
      ..
    }
  ) {
    return Err(CommandErrorDto::new(
      "ssh_master_selection_unsupported",
      "SSH master selection currently requires macOS or Linux.",
    ));
  }
  target.to_ssh_target().map_err(Into::into)
}

async fn request(message: ClientMessage) -> CommandResult<ServerMessage> {
  connection::request(message)
    .await
    .map_err(|error| match error {
      Error::Daemon { code, message } => CommandErrorDto::new(code, message),
      error @ Error::UnsupportedGatewayRoute(_) => {
        CommandErrorDto::new("ctld_protocol_feature_unsupported", error.to_string())
      }
      error @ Error::UnsupportedBrokerProtocol(_) => {
        CommandErrorDto::new("ctld_protocol_version_mismatch", error.to_string())
      }
      error => CommandErrorDto::backend(error),
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn forwarding_rejects_local_targets_before_contacting_the_helper() {
    let forward = LocalPortForward {
      forward_id: "web".into(),
      bind_address: "127.0.0.1".into(),
      local_port: 8080,
      remote_host: "localhost".into(),
      remote_port: 80,
    };
    assert_eq!(
      configure_port_forward(&ConnectionTargetDto::Local, forward, true)
        .await
        .unwrap_err()
        .code,
      "invalid_ssh_target",
    );
    assert_eq!(
      list_port_forwards(&ConnectionTargetDto::Local)
        .await
        .unwrap_err()
        .code,
      "invalid_ssh_target",
    );
  }

  #[cfg(not(unix))]
  #[tokio::test]
  async fn explicit_master_selection_is_rejected_before_helper_startup() {
    let target = serde_json::from_value(serde_json::json!({
      "kind": "ssh", "destination": "configured", "ssh_config_alias": "configured",
      "use_ssh_config_master": true,
    }))
    .unwrap();
    assert_eq!(
      list_port_forwards(&target).await.unwrap_err().code,
      "ssh_master_selection_unsupported",
    );
  }
}
