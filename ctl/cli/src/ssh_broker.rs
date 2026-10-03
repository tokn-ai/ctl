use std::io::{self, Write as _};
use std::path::PathBuf;

use ctl_core::protocol::ProtocolVersion;
use ctl_ipc::{ClientMessage, PromptKind, ServerMessage, SshTarget};
use zeroize::Zeroizing;

pub async fn ensure_master(target: SshTarget) -> Result<PathBuf, Error> {
  let mut stream = ctl_ipc::connect_or_start_daemon().await?;
  let protocol = handshake(&mut stream).await?;
  validate_route(&target, protocol)?;
  ctl_ipc::write_frame(&mut stream, &ClientMessage::EnsureMaster { target }).await?;
  loop {
    match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
        warning,
      }) => {
        let response = tokio::task::spawn_blocking(move || {
          if let Some(warning) = warning {
            eprintln!("Warning: {warning}");
          }
          prompt(kind, &message)
        })
        .await
        .map_err(|_| Error::PromptWorkerStopped)??;
        ctl_ipc::write_frame(
          &mut stream,
          &ClientMessage::PromptResponse {
            prompt_id,
            response,
          },
        )
        .await?;
      }
      Some(ServerMessage::MasterReady { control_path }) => return Ok(control_path),
      Some(ServerMessage::AuthenticationRequired) => return Err(Error::AuthenticationRequired),
      Some(ServerMessage::Error { code, message }) => return Err(Error::Daemon { code, message }),
      Some(_) => return Err(Error::UnexpectedResponse),
      None => return Err(Error::ConnectionClosed),
    }
  }
}

pub async fn request(message: ClientMessage) -> Result<ServerMessage, Error> {
  let mut stream = ctl_ipc::connect_or_start_daemon().await?;
  let protocol = handshake(&mut stream).await?;
  validate_request_contract(&message, protocol)?;
  ctl_ipc::write_frame(&mut stream, &message).await?;
  match ctl_ipc::read_frame::<_, ServerMessage>(&mut stream).await? {
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    Some(message) => Ok(message),
    None => Err(Error::ConnectionClosed),
  }
}

/// Exchange a passive observation without starting ctld or authenticating SSH.
pub async fn request_existing(message: ClientMessage) -> Result<Option<ServerMessage>, Error> {
  let exchange = async {
    let mut stream = match ctl_ipc::connect_existing().await {
      Ok(stream) => stream,
      Err(ctl_ipc::ConnectError::Connect(error))
        if matches!(
          error.kind(),
          io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) =>
      {
        return Ok(None);
      }
      Err(error) => return Err(error.into()),
    };
    let protocol = handshake(&mut stream).await?;
    validate_request_contract(&message, protocol)?;
    ctl_ipc::write_frame(&mut stream, &message).await?;
    let response = ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
      .await?
      .ok_or(Error::ConnectionClosed)?;
    Ok(Some(response))
  };
  tokio::time::timeout(std::time::Duration::from_secs(15), exchange)
    .await
    .map_err(|_| Error::StatusTimeout)?
}

/// Check the complete route before starting any prerequisite VPNs or SSH masters.
pub async fn check_route_support(target: &SshTarget) -> Result<(), Error> {
  if !ctl_ipc::has_remote_vpn(&target.gateways) {
    return Ok(());
  }
  let mut stream = ctl_ipc::connect_or_start_daemon().await?;
  validate_route(target, handshake(&mut stream).await?)
}

fn validate_request_contract(
  message: &ClientMessage,
  protocol: ProtocolVersion,
) -> Result<(), Error> {
  match message {
    ClientMessage::EnsureMaster { target }
    | ClientMessage::MasterStatus { target }
    | ClientMessage::ConnectionStatus { target }
    | ClientMessage::DisconnectMaster { target }
    | ClientMessage::DeleteCredentials { target }
    | ClientMessage::ConfigurePortForward { target, .. }
    | ClientMessage::ListPortForwards { target }
    | ClientMessage::ListRemoteListeners { target } => validate_route(target, protocol),
    _ => Ok(()),
  }
}

fn validate_route(target: &SshTarget, protocol: ProtocolVersion) -> Result<(), Error> {
  if ctl_ipc::gateway_route_supported(&target.gateways, protocol) {
    Ok(())
  } else {
    Err(Error::UnsupportedGatewayRoute(protocol))
  }
}

async fn handshake<S>(stream: &mut S) -> Result<ProtocolVersion, Error>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
  ctl_ipc::write_frame(
    stream,
    &ClientMessage::Handshake {
      protocol: ctl_ipc::protocol_offer(),
    },
  )
  .await?;
  match ctl_ipc::read_frame::<_, ServerMessage>(stream).await? {
    Some(ServerMessage::HandshakeAccepted { protocol_version })
      if ctl_ipc::protocol_offer().accepts(protocol_version) =>
    {
      Ok(protocol_version)
    }
    None => Err(Error::ConnectionClosed),
    _ => Err(Error::UnexpectedResponse),
  }
}

fn prompt(kind: PromptKind, message: &str) -> Result<Option<Zeroizing<String>>, Error> {
  match kind {
    PromptKind::Secret => rpassword::prompt_password(format!("{message} "))
      .map(Zeroizing::new)
      .map(Some)
      .map_err(Error::Prompt),
    PromptKind::Confirm => {
      eprint!("{message} ");
      read_response().map(|response| Some(Zeroizing::new(response)))
    }
    PromptKind::CredentialSave => {
      eprintln!("{message}");
      eprint!("Save credential? [yes/no/never] ");
      read_response().map(|response| Some(Zeroizing::new(response.to_lowercase())))
    }
    PromptKind::CredentialSaveError => {
      eprintln!("{message}");
      eprint!("Press Enter to continue. ");
      read_response().map(|_| Some(Zeroizing::new("confirm".into())))
    }
  }
}

fn read_response() -> Result<String, Error> {
  use std::io::BufRead as _;
  io::stderr().flush().map_err(Error::Prompt)?;
  // stdin may be an scp protocol stream or the input to ctl exec. Prompts must
  // never consume those bytes or wait forever for binary input to end.
  let terminal = std::fs::File::open("/dev/tty").map_err(Error::Prompt)?;
  let mut response = Zeroizing::new(String::new());
  io::BufReader::new(terminal)
    .read_line(&mut response)
    .map_err(Error::Prompt)?;
  while response.ends_with(['\n', '\r']) {
    response.pop();
  }
  Ok(std::mem::take(&mut *response))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("ctld status query timed out")]
  StatusTimeout,
  #[error(transparent)]
  Connect(#[from] ctl_ipc::ConnectError),
  #[error(transparent)]
  Codec(#[from] ctl_ipc::CodecError),
  #[error("ctld closed the SSH authentication request")]
  ConnectionClosed,
  #[error("ctld returned an unexpected response")]
  UnexpectedResponse,
  #[error(
    "Remote VPN routes require local protocol 1.1.13, but ctld selected {0}. Rebuild or update ctld, then restart ctld."
  )]
  UnsupportedGatewayRoute(ProtocolVersion),
  #[error("ctld requires a new explicit SSH authentication attempt")]
  AuthenticationRequired,
  #[error("ctld error {code}: {message}")]
  Daemon { code: String, message: String },
  #[error("could not read an SSH response: {0}")]
  Prompt(#[source] io::Error),
  #[error("the SSH prompt reader stopped unexpectedly")]
  PromptWorkerStopped,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn broker_closing_during_handshake_is_a_transport_failure() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
    });
    assert!(matches!(
      handshake(&mut client).await,
      Err(Error::ConnectionClosed)
    ));
    daemon.await.unwrap();
  }

  #[tokio::test]
  async fn unexpected_handshake_response_remains_a_protocol_failure() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
        .await
        .unwrap();
      ctl_ipc::write_frame(&mut server, &ServerMessage::AuthenticationRequired)
        .await
        .unwrap();
    });
    assert!(matches!(
      handshake(&mut client).await,
      Err(Error::UnexpectedResponse)
    ));
    daemon.await.unwrap();
  }

  #[tokio::test]
  async fn negotiated_contract_retains_historical_routes_and_guards_remote_operations() {
    let mut target: SshTarget = serde_json::from_value(serde_json::json!({
      "destination":"office", "hostname":null, "user":null, "port":null, "identity_file":null
    }))
    .unwrap();
    let ssh = serde_json::json!({"destination":"bastion", "hostname":null, "user":null, "port":null, "identity_file":null, "mode":"automatic"});
    let vpn = serde_json::json!({"kind":"vpn", "destination":"work", "hostname":null, "user":null, "port":null, "identity_file":null, "mode":"automatic", "vpn":{"connection_id":"work", "socket_path":"/tmp/test-vpn.sock"}});
    for selected in [ctl_ipc::CONTRACT_V1_0_12, ctl_ipc::CONTRACT_V1_1_13] {
      let (mut client, mut server) = tokio::io::duplex(4096);
      let daemon = tokio::spawn(async move {
        assert!(
          matches!(ctl_ipc::read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
          Some(ClientMessage::Handshake {protocol}) if protocol.accepts(selected))
        );
        ctl_ipc::write_frame(
          &mut server,
          &ServerMessage::HandshakeAccepted {
            protocol_version: selected,
          },
        )
        .await
        .unwrap();
      });
      let protocol = handshake(&mut client).await.unwrap();
      assert_eq!(protocol, selected);
      daemon.await.unwrap();
      for route in [
        serde_json::json!([]),
        serde_json::json!([ssh.clone()]),
        serde_json::json!([vpn.clone(), ssh.clone()]),
      ] {
        target.gateways = serde_json::from_value(route).unwrap();
        validate_request_contract(
          &ClientMessage::MasterStatus {
            target: target.clone(),
          },
          protocol,
        )
        .unwrap();
      }
      target.gateways =
        serde_json::from_value(serde_json::json!([ssh.clone(), vpn.clone()])).unwrap();
      let result = validate_request_contract(
        &ClientMessage::EnsureMaster {
          target: target.clone(),
        },
        protocol,
      );
      if selected == ctl_ipc::CONTRACT_V1_0_12 {
        assert!(matches!(
          result,
          Err(Error::UnsupportedGatewayRoute(ctl_ipc::CONTRACT_V1_0_12))
        ));
      } else {
        result.unwrap();
      }
    }
  }
}
