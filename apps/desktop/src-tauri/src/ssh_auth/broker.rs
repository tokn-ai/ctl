use ctl_core::protocol::ProtocolVersion;
use std::collections::HashSet;
use std::path::PathBuf;

use ctl_ipc::{
  ClientMessage, LocalPortForward, PortForwardStatus, PromptKind, ServerMessage, SshTarget,
};

use super::{PromptContext, SshPromptKind, request_response};
use crate::dto::{ConnectionTargetDto, SshConnectionStatusDto};
use crate::error::{CommandErrorDto, CommandResult};

pub async fn ensure_master(
  target: &ConnectionTargetDto,
  context: &PromptContext,
) -> CommandResult<PathBuf> {
  ensure_target_master(broker_target(target)?, context).await
}

pub(crate) async fn ensure_target_master(
  target: SshTarget,
  context: &PromptContext,
) -> CommandResult<PathBuf> {
  let mut stream = connect(&target).await?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::EnsureMaster { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  loop {
    match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
      .await
      .map_err(CommandErrorDto::backend)?
    {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
        warning,
      }) => {
        let response = request_response(Some(context), prompt_kind(kind), message, warning).await;
        ctl_ipc::write_frame(
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
  let target = broker_target(target)?;
  let mut stream = connect_existing(&target)
    .await?
    .ok_or_else(authentication_required)?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::MasterStatus { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
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

pub async fn connection_status(
  target: &ConnectionTargetDto,
) -> CommandResult<SshConnectionStatusDto> {
  let target = broker_target(target)?;
  let Some(mut stream) = connect_existing(&target).await? else {
    return Ok(SshConnectionStatusDto::default());
  };
  ctl_ipc::write_frame(&mut stream, &ClientMessage::ConnectionStatus { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  let response = ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?;
  connection_status_response(response)
}

fn connection_status_response(
  response: Option<ServerMessage>,
) -> CommandResult<SshConnectionStatusDto> {
  match response {
    Some(ServerMessage::ConnectionStatus {
      connected,
      manually_disconnected,
    }) => Ok(SshConnectionStatusDto {
      connected,
      manually_disconnected,
    }),
    Some(ServerMessage::MasterReady { .. }) => Ok(SshConnectionStatusDto {
      connected: true,
      manually_disconnected: false,
    }),
    Some(ServerMessage::AuthenticationRequired) => Ok(SshConnectionStatusDto::default()),
    Some(ServerMessage::Error { code, .. }) if code == "ssh_host_disconnected" => {
      Ok(SshConnectionStatusDto {
        connected: false,
        manually_disconnected: true,
      })
    }
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected SSH connection status.",
    )),
  }
}

pub async fn disconnect(targets: &[ConnectionTargetDto]) -> CommandResult<()> {
  disconnect_targets(targets, disconnect_master).await
}

async fn disconnect_targets<F: std::future::Future<Output = CommandResult<()>>>(
  targets: &[ConnectionTargetDto],
  mut disconnect_one: impl FnMut(SshTarget) -> F,
) -> CommandResult<()> {
  let mut seen = HashSet::new();
  let mut failures = Vec::new();
  for target in targets {
    match broker_target(target) {
      Ok(target) if seen.insert(target.clone()) => {
        if let Err(error) = disconnect_one(target.clone()).await {
          failures.push(format!("{}: {}", target.destination, error.message));
        }
      }
      Ok(_) => {}
      Err(error) => failures.push(error.message),
    }
  }
  if failures.is_empty() {
    Ok(())
  } else {
    Err(CommandErrorDto::new(
      "ssh_disconnect_failed",
      failures.join("\n"),
    ))
  }
}

async fn disconnect_master(target: SshTarget) -> CommandResult<()> {
  let mut stream = connect(&target).await?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::DisconnectMaster { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::MasterDisconnected) => Ok(()),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected SSH disconnect response.",
    )),
  }
}

pub async fn delete_credentials(target: &ConnectionTargetDto) -> CommandResult<()> {
  let target = broker_target(target)?;
  let mut stream = connect(&target).await?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::DeleteCredentials { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
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
  let target = broker_target(target)?;
  let mut stream = connect(&target).await?;
  ctl_ipc::write_frame(
    &mut stream,
    &ClientMessage::ConfigurePortForward {
      target,
      forward,
      enabled,
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
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
  let target = broker_target(target)?;
  let mut stream = connect(&target).await?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::ListPortForwards { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
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
  let target = broker_target(target)?;
  let mut stream = connect_existing(&target)
    .await?
    .ok_or_else(authentication_required)?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::ListRemoteListeners { target })
    .await
    .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
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

/// Preflight the complete chain before starting its VPN prerequisites.
pub(crate) async fn check_route_support(target: &SshTarget) -> CommandResult<()> {
  if ctl_ipc::has_remote_vpn(&target.gateways) {
    connect(target).await?;
  }
  Ok(())
}

async fn connect(target: &SshTarget) -> CommandResult<ctl_ipc::Stream> {
  let mut stream = ctl_ipc::connect_or_start_daemon()
    .await
    .map_err(CommandErrorDto::backend)?;
  let protocol = handshake(&mut stream).await?;
  validate_route(target, protocol)?;
  Ok(stream)
}

async fn connect_existing(target: &SshTarget) -> CommandResult<Option<ctl_ipc::Stream>> {
  let mut stream = match ctl_ipc::connect_existing().await {
    Ok(stream) => stream,
    Err(ctl_ipc::ConnectError::Connect(error))
      if matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
      ) =>
    {
      return Ok(None);
    }
    Err(error) => return Err(CommandErrorDto::backend(error)),
  };
  let protocol = handshake(&mut stream).await?;
  validate_route(target, protocol)?;
  Ok(Some(stream))
}

fn validate_route(target: &SshTarget, protocol: ProtocolVersion) -> CommandResult<()> {
  if ctl_ipc::gateway_route_supported(&target.gateways, protocol) {
    Ok(())
  } else {
    Err(CommandErrorDto::new(
      "ctld_protocol_feature_unsupported",
      format!(
        "Remote VPN routes require local protocol 1.1.13, but ctld selected {protocol}. Rebuild or update ctld to match the app, then restart ctld."
      ),
    ))
  }
}

async fn handshake<S>(stream: &mut S) -> CommandResult<ProtocolVersion>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  ctl_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol: ctl_ipc::protocol_offer(),
    },
  )
  .await
  .map_err(CommandErrorDto::backend)?;
  match ctl_ipc::read_frame::<_, ServerMessage>(stream)
    .await
    .map_err(CommandErrorDto::backend)?
  {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if ctl_ipc::protocol_offer().accepts(protocol_version) =>
    {
      Ok(protocol_version)
    }
    Some(ServerMessage::HandshakeAccepted { protocol_version }) => Err(CommandErrorDto::new(
      "ctld_protocol_version_mismatch",
      format!(
        "The app requires local protocol {}, but ctld accepted protocol {protocol_version}. Rebuild or update ctld to match the app, then restart ctld.",
        ctl_ipc::PROTOCOL_VERSION,
      ),
    )),
    Some(ServerMessage::Error { code, message }) => Err(CommandErrorDto::new(code, message)),
    None => Err(CommandErrorDto::new(
      "ctld_connection_closed",
      format!(
        "ctld closed the local handshake before replying to app protocol {}. A stale local ctld may be running. Rebuild or update ctld to match the app, then restart ctld.",
        ctl_ipc::PROTOCOL_VERSION,
      ),
    )),
    _ => Err(CommandErrorDto::new(
      "ctld_protocol_error",
      "ctld returned an unexpected local handshake response. Rebuild or update ctld to match the app, then restart ctld.",
    )),
  }
}

pub(super) fn broker_target(target: &ConnectionTargetDto) -> CommandResult<SshTarget> {
  target.to_ssh_target().map_err(Into::into)
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn vpn_routes_keep_stable_identity_in_authentication_and_core_transport() {
    let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "office",
      "vpn_connection_id": "work-vpn",
      "gateways": [{
        "gateway_id": "bastion",
        "name": "Bastion",
        "destination": "jump",
        "mode": "automatic"
      }]
    }))
    .unwrap();
    let broker = broker_target(&target).unwrap();
    assert_eq!(broker.gateways.len(), 2);
    let vpn = &broker.gateways[0];
    assert!(vpn.has_valid_vpn_configuration());
    assert_eq!(vpn.kind, ctl_ipc::GatewayKind::Vpn);
    assert_eq!(vpn.destination, "work-vpn");
    assert_eq!(vpn.port, None);
    assert!(vpn.vpn.as_ref().unwrap().socket_path.is_absolute());
    assert_eq!(broker.gateways[1].destination, "jump");
    let ctl_client::ConnectionTarget::Ssh { options, .. } = target.to_core() else {
      panic!("expected SSH target")
    };
    assert_eq!(options.gateways[0].vpn, vpn.vpn);
    assert_eq!(options.gateways[1].destination, "jump");
    assert_eq!(broker_target(&target).unwrap(), broker);
    let serialized = serde_json::to_value(&target).unwrap();
    assert_eq!(serialized["vpn_connection_id"], "work-vpn");
    assert!(serialized.get("socket_path").is_none());
  }

  #[test]
  fn malformed_vpn_references_never_become_direct_connections() {
    for value in ["", "contains whitespace", "line\nfeed"] {
      let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
        "kind": "ssh", "destination": "office", "vpn_connection_id": value,
      }))
      .unwrap();
      assert_eq!(
        broker_target(&target).unwrap_err().code,
        "invalid_vpn_route"
      );
    }
  }

  #[test]
  fn broker_target_preserves_ssh_config_origin() {
    let target: ConnectionTargetDto = serde_json::from_value(serde_json::json!({
      "kind": "ssh",
      "destination": "office",
      "ssh_config_alias": "office",
      "user": "alice"
    }))
    .unwrap();
    let target = broker_target(&target).unwrap();
    assert_eq!(target.ssh_config_alias.as_deref(), Some("office"));
    assert_eq!(target.user.as_deref(), Some("alice"));
  }

  #[test]
  fn broker_target_normalizes_defaults_and_preserves_explicit_policy_overrides() {
    for (alias, policy, expected) in [
      (None, None, None),
      (None, Some(false), None),
      (None, Some(true), Some(true)),
      (Some("office"), None, None),
      (Some("office"), Some(true), None),
      (Some("office"), Some(false), Some(false)),
    ] {
      let mut value = serde_json::json!({
        "kind": "ssh",
        "destination": "office",
      });
      if let Some(alias) = alias {
        value["ssh_config_alias"] = alias.into();
      }
      if let Some(policy) = policy {
        value["use_ssh_config_master"] = policy.into();
      }
      let target: ConnectionTargetDto = serde_json::from_value(value).unwrap();
      let target = broker_target(&target).unwrap();
      assert_eq!(target.ssh_config_alias.as_deref(), alias);
      assert_eq!(target.use_ssh_config_master, expected);
    }
  }

  #[test]
  fn connection_status_preserves_actual_connectivity_and_manual_pause_independently() {
    for connected in [false, true] {
      for manually_disconnected in [false, true] {
        assert_eq!(
          connection_status_response(Some(ServerMessage::ConnectionStatus {
            connected,
            manually_disconnected,
          }))
          .unwrap(),
          SshConnectionStatusDto {
            connected,
            manually_disconnected
          }
        );
      }
    }
    let paused = connection_status_response(Some(ServerMessage::Error {
      code: "ssh_host_disconnected".into(),
      message: "manually paused".into(),
    }))
    .unwrap();
    assert!(!paused.connected);
    assert!(paused.manually_disconnected);
    let error = connection_status_response(Some(ServerMessage::Error {
      code: "ctld_protocol_error".into(),
      message: "invalid target".into(),
    }))
    .unwrap_err();
    assert_eq!(error.code, "ctld_protocol_error");
  }

  #[tokio::test]
  async fn disconnect_deduplicates_routes_and_attempts_every_target_after_failures() {
    let target = |destination: &str| {
      serde_json::from_value::<ConnectionTargetDto>(serde_json::json!({
        "kind": "ssh", "destination": destination,
      }))
      .unwrap()
    };
    let mut attempted = Vec::new();
    let error = disconnect_targets(
      &[
        target("first"),
        target("first"),
        ConnectionTargetDto::Local,
        target("second"),
        target("third"),
      ],
      |target| {
        attempted.push(target.destination.clone());
        std::future::ready(if target.destination == "second" {
          Ok(())
        } else {
          Err(CommandErrorDto::new("test_failure", "exit failed"))
        })
      },
    )
    .await
    .unwrap_err();
    assert_eq!(attempted, ["first", "second", "third"]);
    assert_eq!(error.code, "ssh_disconnect_failed");
    assert!(error.message.contains("first: exit failed"));
    assert!(error.message.contains("third: exit failed"));
    assert!(error.message.contains("Select a remote SSH host"));
    assert!(!error.message.contains("second"));
  }

  async fn handshake_reply(reply: Option<ServerMessage>) -> CommandResult<ProtocolVersion> {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(ClientMessage::Handshake { protocol })
          if protocol.negotiate(ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS).is_some()
      ));
      if let Some(reply) = reply {
        ctl_ipc::write_frame(&mut server, &reply).await.unwrap();
      }
    });
    let result = handshake(&mut client).await;
    daemon.await.unwrap();
    result
  }

  #[tokio::test]
  async fn handshake_accepts_the_matching_protocol() {
    handshake_reply(Some(ServerMessage::HandshakeAccepted {
      protocol_version: ctl_ipc::PROTOCOL_VERSION,
    }))
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn historical_contract_retains_local_routes_and_rejects_remote_vpn_routes() {
    let selected = handshake_reply(Some(ServerMessage::HandshakeAccepted {
      protocol_version: ctl_ipc::CONTRACT_V1_0_12,
    }))
    .await
    .unwrap();
    assert_eq!(selected, ctl_ipc::CONTRACT_V1_0_12);
    let mut target = ConnectionTargetDto::ssh("office").to_ssh_target().unwrap();
    validate_route(&target, selected).unwrap();
    let ssh: ctl_ipc::SshGateway = serde_json::from_value(serde_json::json!({
      "destination":"bastion", "hostname":null, "user":null, "port":null, "identity_file":null, "mode":"automatic"
    })).unwrap();
    let vpn: ctl_ipc::SshGateway = serde_json::from_value(serde_json::json!({
      "kind":"vpn", "destination":"work", "hostname":null, "user":null, "port":null, "identity_file":null, "mode":"automatic",
      "vpn":{"connection_id":"work", "socket_path":"/tmp/test-vpn.sock"}
    })).unwrap();
    target.gateways = vec![ssh.clone()];
    validate_route(&target, selected).unwrap();
    target.gateways = vec![vpn.clone(), ssh.clone()];
    validate_route(&target, selected).unwrap();
    target.gateways = vec![ssh, vpn];
    let error = validate_route(&target, selected).unwrap_err();
    assert_eq!(error.code, "ctld_protocol_feature_unsupported");
    assert!(error.message.contains("1.1.13") && error.message.contains("1.0.12"));
    validate_route(&target, ctl_ipc::CONTRACT_V1_1_13).unwrap();
  }

  #[tokio::test]
  async fn handshake_preserves_the_daemon_rejection() {
    let error = handshake_reply(Some(ServerMessage::Error {
      code: "ctld_protocol_version_mismatch".into(),
      message: "ctld requires local protocol 3, but the client requested 4.".into(),
    }))
    .await
    .unwrap_err();
    assert_eq!(error.code, "ctld_protocol_version_mismatch");
    assert_eq!(
      error.message,
      "ctld requires local protocol 3, but the client requested 4."
    );
  }

  #[tokio::test]
  async fn handshake_rejects_a_different_accepted_version() {
    let old_version = ctl_core::protocol::ProtocolVersion::new(1, 0, 11);
    let error = handshake_reply(Some(ServerMessage::HandshakeAccepted {
      protocol_version: old_version,
    }))
    .await
    .unwrap_err();
    assert_eq!(error.code, "ctld_protocol_version_mismatch");
    assert!(error.message.contains(&format!(
      "requires local protocol {}",
      ctl_ipc::PROTOCOL_VERSION
    )));
    assert!(
      error
        .message
        .contains(&format!("ctld accepted protocol {old_version}"))
    );
    assert!(error.message.contains("restart ctld"));
  }

  #[tokio::test]
  async fn legacy_handshake_eof_explains_how_to_recover() {
    let error = handshake_reply(None).await.unwrap_err();
    assert_eq!(error.code, "ctld_connection_closed");
    assert!(error.message.contains("closed the local handshake"));
    assert!(error.message.contains("Rebuild or update ctld"));
    assert!(error.message.contains("restart ctld"));
  }
}
