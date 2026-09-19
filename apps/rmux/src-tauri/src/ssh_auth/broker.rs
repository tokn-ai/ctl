use std::path::PathBuf;

use ctld_ipc::{
  ClientMessage, LocalPortForward, PortForwardStatus, PromptKind, ServerMessage, SshGateway,
  SshGatewayMode, SshTarget,
};

use super::{PromptContext, SshPromptKind, request_response};
use crate::dto::ConnectionTargetDto;
use crate::error::{CommandErrorDto, CommandResult};

pub async fn ensure_master(
  target: &ConnectionTargetDto,
  context: &PromptContext,
) -> CommandResult<PathBuf> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::EnsureMaster {
      target: broker_target(target)?,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  loop {
    match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
      .await
      .map_err(CommandErrorDto::backend)?
    {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
      }) => {
        let response = request_response(Some(context), prompt_kind(kind), message).await;
        ctld_ipc::write_frame(
          &mut stream,
          &ClientMessage::PromptResponse {
            prompt_id,
            response,
          },
        )
        .await
        .map_err(CommandErrorDto::backend)?;
      }
      Some(ServerMessage::MasterReady { control_path }) => return Ok(control_path),
      Some(ServerMessage::AuthenticationRequired) => return Err(authentication_required()),
      Some(ServerMessage::Error { code, message }) => {
        return Err(CommandErrorDto::new(code, message));
      }
      Some(_) => {
        return Err(CommandErrorDto::new(
          "ctld_protocol_error",
          "ctld returned an unexpected response while connecting SSH.",
        ));
      }
      None => {
        return Err(CommandErrorDto::new(
          "ctld_connection_closed",
          "ctld closed the SSH authentication request.",
        ));
      }
    }
  }
}

pub async fn existing_master(target: &ConnectionTargetDto) -> CommandResult<PathBuf> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::MasterStatus {
      target: broker_target(target)?,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::MasterReady { control_path }) => Ok(control_path),
    Some(ServerMessage::AuthenticationRequired) => Err(authentication_required()),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected SSH master status.",
    )),
  }
}

pub async fn delete_credentials(target: &ConnectionTargetDto) -> CommandResult<()> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::DeleteCredentials {
      target: broker_target(target)?,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::CredentialsDeleted) => Ok(()),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected credential deletion response.",
    )),
  }
}

pub async fn configure_port_forward(
  target: &ConnectionTargetDto,
  forward: LocalPortForward,
  enabled: bool,
) -> CommandResult<PortForwardStatus> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::ConfigurePortForward {
      target: broker_target(target)?,
      forward,
      enabled,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::PortForwardConfigured { status }) => Ok(status),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected port-forward response.",
    )),
  }
}

pub async fn list_port_forwards(
  target: &ConnectionTargetDto,
) -> CommandResult<Vec<PortForwardStatus>> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::ListPortForwards {
      target: broker_target(target)?,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::PortForwards { statuses }) => Ok(statuses),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected port-forward list.",
    )),
  }
}

pub async fn list_remote_listeners(
  target: &ConnectionTargetDto,
) -> CommandResult<ctl_proto::TcpListenerCatalog> {
  let mut stream = connect().await?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::ListRemoteListeners {
      target: broker_target(target)?,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::RemoteListeners { catalog }) => Ok(catalog),
    Some(ServerMessage::AuthenticationRequired) => Err(authentication_required()),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected remote-listener response.",
    )),
  }
}

async fn connect() -> CommandResult<ctld_ipc::Stream> {
  let mut stream = ctld_ipc::connect_or_start_daemon()
    .await
    .map_err(CommandErrorDto::backend)?;
  ctld_ipc::write_frame(
    &mut stream,
    &ClientMessage::Handshake {
      protocol_version: ctld_ipc::PROTOCOL_VERSION,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctld_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if protocol_version == ctld_ipc::PROTOCOL_VERSION =>
    {
      Ok(stream)
    }
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld did not accept the local protocol handshake.",
    )),
  }
}

fn broker_target(target: &ConnectionTargetDto) -> CommandResult<SshTarget> {
  match target {
    ConnectionTargetDto::Ssh {
      destination,
      hostname,
      user,
      port,
      identity_file,
      gateways,
      ..
    } => Ok(SshTarget {
      destination: destination.clone(),
      hostname: hostname.clone(),
      user: user.clone(),
      port: *port,
      identity_file: identity_file.as_ref().map(PathBuf::from),
      gateways: gateways
        .iter()
        .map(|gateway| SshGateway {
          destination: gateway.destination.clone(),
          hostname: gateway.hostname.clone(),
          user: gateway.user.clone(),
          port: gateway.port,
          identity_file: gateway.identity_file.as_ref().map(PathBuf::from),
          mode: match gateway.mode {
            crate::dto::SshGatewayModeDto::Automatic => SshGatewayMode::Automatic,
            crate::dto::SshGatewayModeDto::NativeOnly => SshGatewayMode::NativeOnly,
            crate::dto::SshGatewayModeDto::AgentRelayOnly => SshGatewayMode::AgentRelayOnly,
          },
        })
        .collect(),
    }),
    ConnectionTargetDto::Local => Err(CommandErrorDto::new(
      "invalid_ssh_target",
      "Select a remote SSH host.",
    )),
  }
}

fn prompt_kind(kind: PromptKind) -> SshPromptKind {
  match kind {
    PromptKind::Confirm => SshPromptKind::Confirm,
    PromptKind::Secret => SshPromptKind::Secret,
    PromptKind::CredentialSave => SshPromptKind::CredentialSave,
    PromptKind::CredentialSaveError => SshPromptKind::CredentialSaveError,
  }
}

fn authentication_required() -> CommandErrorDto {
  CommandErrorDto::new(
    "ssh_authentication_required",
    "SSH authentication is required. Use Connect host to authenticate.",
  )
}
