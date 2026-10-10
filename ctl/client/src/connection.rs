//! Shared ctld connection API. Frontends own prompt presentation.

use std::io;
use std::path::PathBuf;

use ctl_core::protocol::ProtocolVersion;
use ctl_ipc::{ClientMessage, PromptKind, ServerMessage, SshTarget};
use zeroize::Zeroizing;

/// Whether this explicit connection action may invoke a frontend prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionPolicy {
  Quiet,
  Interactive,
}

/// Passive connection state, including explicit user disconnect policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectionStatus {
  pub connected: bool,
  pub manually_disconnected: bool,
}

/// An authenticated master usable by subsequent OpenSSH channels.
#[derive(Debug)]
pub struct ConnectionReady {
  pub control_path: PathBuf,
}

/// One request at a time; taking the socket makes cancellation close it rather
/// than returning a partially consumed response to the next caller.
#[derive(Default)]
pub struct ConnectionClient {
  connection: tokio::sync::Mutex<Option<(ctl_ipc::Stream, ProtocolVersion)>>,
}

impl ConnectionClient {
  /// Observe SSH without starting ctld or establishing a master.
  /// # Errors
  /// Returns transport, negotiation, or invalid response errors.
  pub async fn status(&self, target: SshTarget) -> Result<ConnectionStatus, Error> {
    let response = tokio::time::timeout(
      std::time::Duration::from_secs(15),
      self.exchange(ClientMessage::ConnectionStatus { target }, true, false),
    )
    .await
    .map_err(|_| Error::StatusTimeout)??;
    status_response(response)
  }

  /// Close the selected SSH master without terminating persistent sessions.
  /// # Errors
  /// Returns transport, negotiation, or daemon errors; mutations are not retried.
  pub async fn disconnect(&self, target: SshTarget) -> Result<(), Error> {
    match self
      .exchange(ClientMessage::DisconnectMaster { target }, false, true)
      .await?
    {
      Some(ServerMessage::MasterDisconnected) => Ok(()),
      Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
      _ => Err(Error::UnexpectedResponse),
    }
  }

  async fn exchange(
    &self,
    message: ClientMessage,
    existing_only: bool,
    interrupt: bool,
  ) -> Result<Option<ServerMessage>, Error> {
    self
      .exchange_using(message, interrupt, async {
        let stream = if existing_only {
          match ctl_ipc::connect_existing().await {
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
          }
        } else {
          ctl_ipc::connect_or_start_daemon().await?
        };
        Ok(Some(stream))
      })
      .await
  }

  async fn exchange_using<F>(
    &self,
    message: ClientMessage,
    interrupt: bool,
    connect: F,
  ) -> Result<Option<ServerMessage>, Error>
  where
    F: std::future::Future<Output = Result<Option<ctl_ipc::Stream>, Error>>,
  {
    // Disconnect must be able to cancel an ensure waiting on a frontend prompt.
    // Use a separate temporary socket when that ensure owns the reusable one.
    let mut slot = if interrupt {
      self.connection.try_lock().ok()
    } else {
      Some(self.connection.lock().await)
    };
    let (mut stream, protocol) =
      if let Some(connection) = slot.as_mut().and_then(|slot| slot.take()) {
        connection
      } else {
        let Some(mut stream) = connect.await? else {
          return Ok(None);
        };
        let protocol = handshake(&mut stream).await?;
        (stream, protocol)
      };
    validate_request_contract(&message, protocol)?;
    ctl_ipc::write_frame(&mut stream, &message).await?;
    let response = ctl_ipc::read_frame::<_, ServerMessage>(&mut stream)
      .await?
      .ok_or(Error::ConnectionClosed)?;
    if ctl_ipc::persistent_requests_supported(protocol)
      && !matches!(response, ServerMessage::Error { .. })
      && let Some(slot) = slot.as_mut()
    {
      **slot = Some((stream, protocol));
    }
    Ok(Some(response))
  }

  /// Ensure a master using frontend-supplied prompts only in interactive mode.
  /// # Errors
  /// Returns authentication, negotiation, transport, or frontend prompt errors.
  pub async fn ensure<F, P>(
    &self,
    target: SshTarget,
    interaction: InteractionPolicy,
    ask: F,
  ) -> Result<ConnectionReady, Error>
  where
    F: FnMut(PromptKind, String, Option<String>) -> P,
    P: std::future::Future<Output = Result<Option<Zeroizing<String>>, Error>>,
  {
    let interactive = interaction == InteractionPolicy::Interactive;
    let mut slot = self.connection.lock().await;
    let (mut stream, protocol) = if let Some(connection) = slot.take() {
      connection
    } else {
      let mut stream = ctl_ipc::connect_or_start_daemon().await?;
      let protocol = handshake(&mut stream).await?;
      (stream, protocol)
    };
    let result = ensure_master_exchange(&mut stream, protocol, target, interactive, ask).await;
    if result.is_ok() && ctl_ipc::persistent_requests_supported(protocol) {
      *slot = Some((stream, protocol));
    }
    result.map(|control_path| ConnectionReady { control_path })
  }
}

fn status_response(response: Option<ServerMessage>) -> Result<ConnectionStatus, Error> {
  match response {
    Some(ServerMessage::ConnectionStatus {
      connected,
      manually_disconnected,
    }) => Ok(ConnectionStatus {
      connected,
      manually_disconnected,
    }),
    Some(ServerMessage::MasterReady { .. }) => Ok(ConnectionStatus {
      connected: true,
      manually_disconnected: false,
    }),
    None | Some(ServerMessage::AuthenticationRequired) => Ok(ConnectionStatus::default()),
    Some(ServerMessage::Error { code, .. }) if code == "ssh_host_disconnected" => {
      Ok(ConnectionStatus {
        connected: false,
        manually_disconnected: true,
      })
    }
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    _ => Err(Error::UnexpectedResponse),
  }
}

#[cfg(test)]
async fn ensure_master_on<S, F, P>(
  stream: &mut S,
  target: SshTarget,
  interactive: bool,
  ask: F,
) -> Result<PathBuf, Error>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
  F: FnMut(PromptKind, String, Option<String>) -> P,
  P: std::future::Future<Output = Result<Option<Zeroizing<String>>, Error>>,
{
  let protocol = handshake(stream).await?;
  ensure_master_exchange(stream, protocol, target, interactive, ask).await
}

async fn ensure_master_exchange<S, F, P>(
  stream: &mut S,
  protocol: ProtocolVersion,
  target: SshTarget,
  interactive: bool,
  mut ask: F,
) -> Result<PathBuf, Error>
where
  S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
  F: FnMut(PromptKind, String, Option<String>) -> P,
  P: std::future::Future<Output = Result<Option<Zeroizing<String>>, Error>>,
{
  validate_route(&target, protocol)?;
  let request = if interactive {
    ClientMessage::EnsureMaster { target }
  } else if ctl_ipc::quiet_master_supported(protocol) {
    ClientMessage::EnsureMasterQuiet { target }
  } else {
    // Earlier brokers cannot suppress their native Keychain UI. Passive reuse
    // is safe; starting a fresh connection must wait for an interactive action.
    ClientMessage::MasterStatus { target }
  };
  ctl_ipc::write_frame(stream, &request).await?;
  loop {
    match ctl_ipc::read_frame::<_, ServerMessage>(stream).await? {
      Some(ServerMessage::Prompt {
        prompt_id,
        kind,
        message,
        warning,
      }) => {
        let authentication_required =
          !interactive && matches!(kind, PromptKind::Secret | PromptKind::Confirm);
        let response = if interactive {
          ask(kind, message, warning).await?
        } else {
          match kind {
            PromptKind::Secret | PromptKind::Confirm => None,
            // Saving a credential is optional after authentication. Declining
            // the offer leaves its existing save policy and live master intact.
            PromptKind::CredentialSave => Some(Zeroizing::new("no".into())),
            PromptKind::CredentialSaveError => Some(Zeroizing::new("confirm".into())),
          }
        };
        ctl_ipc::write_frame(
          stream,
          &ClientMessage::PromptResponse {
            prompt_id,
            response,
          },
        )
        .await?;
        if authentication_required {
          return Err(Error::AuthenticationRequired);
        }
      }
      Some(ServerMessage::MasterReady { control_path }) => return Ok(control_path),
      Some(ServerMessage::AuthenticationRequired) => return Err(Error::AuthenticationRequired),
      Some(ServerMessage::Error { code, message }) => return Err(Error::Daemon { code, message }),
      Some(_) => return Err(Error::UnexpectedResponse),
      None => return Err(Error::ConnectionClosed),
    }
  }
}

/// Exchange an existing non-authentication broker operation.
/// # Errors
/// Returns connection, negotiation, codec, or daemon errors.
pub async fn request(message: ClientMessage) -> Result<ServerMessage, Error> {
  match ConnectionClient::default()
    .exchange(message, false, false)
    .await?
  {
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    Some(response) => Ok(response),
    None => Err(Error::ConnectionClosed),
  }
}

/// Check the complete route before starting any prerequisite VPNs or SSH masters.
/// # Errors
/// Returns broker connection or unsupported route contract errors.
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
    ClientMessage::EnsureMasterQuiet { target } => {
      if !ctl_ipc::quiet_master_supported(protocol) {
        return Err(Error::UnsupportedQuietMaster(protocol));
      }
      validate_route(target, protocol)
    }
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
    Some(ServerMessage::HandshakeAccepted { protocol_version }) => {
      Err(Error::UnsupportedBrokerProtocol(protocol_version))
    }
    Some(ServerMessage::Error { code, message }) => Err(Error::Daemon { code, message }),
    None => Err(Error::ConnectionClosed),
    _ => Err(Error::UnexpectedResponse),
  }
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
    "ctld selected unsupported protocol {0}; rebuild or update the client and ctld, then restart ctld"
  )]
  UnsupportedBrokerProtocol(ProtocolVersion),
  #[error(
    "Remote VPN routes require local protocol 1.1.13, but ctld selected {0}. Rebuild or update ctld, then restart ctld."
  )]
  UnsupportedGatewayRoute(ProtocolVersion),
  #[error(
    "Quiet SSH connection requires local protocol 1.1.14, but ctld selected {0}. Rebuild or update ctld, then restart ctld."
  )]
  UnsupportedQuietMaster(ProtocolVersion),
  #[error(
    "SSH authentication is required. Detach and reconnect to answer the authentication prompt."
  )]
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
  async fn disconnect_can_reach_the_broker_while_authentication_owns_the_socket() {
    let client = ConnectionClient::default();
    let _authentication_guard = client.connection.lock().await;
    let (stream, mut server) = ctl_ipc::Stream::pair().unwrap();
    let peer = tokio::spawn(async move {
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::Handshake { .. })
      ));
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::HandshakeAccepted {
          protocol_version: ctl_ipc::PROTOCOL_VERSION,
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::DisconnectMaster { .. })
      ));
      ctl_ipc::write_frame(&mut server, &ServerMessage::MasterDisconnected)
        .await
        .unwrap();
      assert!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    let response = tokio::time::timeout(
      std::time::Duration::from_secs(1),
      client.exchange_using(
        ClientMessage::DisconnectMaster {
          target: fixture_target(),
        },
        true,
        std::future::ready(Ok(Some(stream))),
      ),
    )
    .await
    .expect("disconnect must not wait for an authentication prompt")
    .unwrap();
    assert!(matches!(response, Some(ServerMessage::MasterDisconnected)));
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn ensure_status_and_disconnect_share_the_negotiated_socket() {
    let client = ConnectionClient::default();
    let (stream, mut server) = ctl_ipc::Stream::pair().unwrap();
    *client.connection.lock().await = Some((stream, ctl_ipc::PROTOCOL_VERSION));
    let peer = tokio::spawn(async move {
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::EnsureMasterQuiet { .. })
      ));
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::MasterReady {
          control_path: "/fixture/master".into(),
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::ConnectionStatus { .. })
      ));
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::ConnectionStatus {
          connected: true,
          manually_disconnected: false,
        },
      )
      .await
      .unwrap();
      assert!(matches!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap(),
        Some(ClientMessage::DisconnectMaster { .. })
      ));
      ctl_ipc::write_frame(&mut server, &ServerMessage::MasterDisconnected)
        .await
        .unwrap();
      assert!(
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    client
      .ensure(fixture_target(), InteractionPolicy::Quiet, no_prompt)
      .await
      .unwrap();
    assert_eq!(
      client.status(fixture_target()).await.unwrap(),
      ConnectionStatus {
        connected: true,
        manually_disconnected: false
      }
    );
    client.disconnect(fixture_target()).await.unwrap();
    drop(client);
    peer.await.unwrap();
  }

  #[test]
  fn passive_status_preserves_connectivity_and_pause_independently() {
    for connected in [false, true] {
      for manually_disconnected in [false, true] {
        assert_eq!(
          status_response(Some(ServerMessage::ConnectionStatus {
            connected,
            manually_disconnected
          }))
          .unwrap(),
          ConnectionStatus {
            connected,
            manually_disconnected
          }
        );
      }
    }
    assert_eq!(status_response(None).unwrap(), ConnectionStatus::default());
    assert_eq!(
      status_response(Some(ServerMessage::Error {
        code: "ssh_host_disconnected".into(),
        message: "paused".into()
      }))
      .unwrap(),
      ConnectionStatus {
        connected: false,
        manually_disconnected: true
      }
    );
    assert!(matches!(
      status_response(Some(ServerMessage::Error {
        code: "unexpected".into(),
        message: "failure".into()
      })),
      Err(Error::Daemon { .. })
    ));
  }

  async fn no_prompt(
    _: PromptKind,
    _: String,
    _: Option<String>,
  ) -> Result<Option<Zeroizing<String>>, Error> {
    panic!("quiet connection must not call the prompt adapter")
  }

  #[tokio::test]
  async fn shared_master_client_serializes_requests_without_new_sockets() {
    let client = std::sync::Arc::new(ConnectionClient::default());
    let (stream, mut server) = ctl_ipc::Stream::pair().unwrap();
    *client.connection.lock().await = Some((stream, ctl_ipc::PROTOCOL_VERSION));
    let worker = tokio::spawn(async move {
      for _ in 0..32 {
        assert!(matches!(
          ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
            .await
            .unwrap(),
          Some(ClientMessage::EnsureMasterQuiet { .. })
        ));
        ctl_ipc::write_frame(
          &mut server,
          &ServerMessage::MasterReady {
            control_path: PathBuf::from("/fixture/master"),
          },
        )
        .await
        .unwrap();
      }
    });
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..32 {
      let client = std::sync::Arc::clone(&client);
      requests.spawn(async move {
        client
          .ensure(fixture_target(), InteractionPolicy::Quiet, no_prompt)
          .await
      });
    }
    while let Some(result) = requests.join_next().await {
      assert_eq!(
        result.unwrap().unwrap().control_path,
        PathBuf::from("/fixture/master")
      );
    }
    worker.await.unwrap();
  }

  #[tokio::test]
  async fn master_client_cancellation_discards_the_socket_and_pending_response() {
    let client = ConnectionClient::default();
    let (stream, mut server) = ctl_ipc::Stream::pair().unwrap();
    *client.connection.lock().await = Some((stream, ctl_ipc::PROTOCOL_VERSION));
    assert!(
      tokio::time::timeout(
        std::time::Duration::from_millis(50),
        client.ensure(fixture_target(), InteractionPolicy::Quiet, no_prompt)
      )
      .await
      .is_err()
    );
    assert!(client.connection.lock().await.is_none());
    assert!(matches!(
      ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
        .await
        .unwrap(),
      Some(ClientMessage::EnsureMasterQuiet { .. })
    ));
    assert!(
      ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
        .await
        .unwrap()
        .is_none()
    );
  }

  #[tokio::test]
  async fn master_client_retires_old_contract_sockets_and_failed_connections() {
    for (protocol, successful) in [
      (ctl_ipc::CONTRACT_V1_1_14, true),
      (ctl_ipc::PROTOCOL_VERSION, false),
    ] {
      let client = ConnectionClient::default();
      let (stream, mut server) = ctl_ipc::Stream::pair().unwrap();
      *client.connection.lock().await = Some((stream, protocol));
      let worker = tokio::spawn(async move {
        ctl_ipc::read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .unwrap();
        if successful {
          ctl_ipc::write_frame(
            &mut server,
            &ServerMessage::MasterReady {
              control_path: "/fixture/master".into(),
            },
          )
          .await
          .unwrap();
        }
      });
      assert_eq!(
        client
          .ensure(fixture_target(), InteractionPolicy::Quiet, no_prompt)
          .await
          .is_ok(),
        successful
      );
      assert!(client.connection.lock().await.is_none());
      worker.await.unwrap();
    }
  }

  fn fixture_target() -> SshTarget {
    crate::hosts::ConnectionTargetDto::ssh("fixture")
      .to_ssh_target()
      .unwrap()
  }

  async fn accept_master_request(server: &mut tokio::io::DuplexStream, interactive: bool) {
    let Some(ClientMessage::Handshake { protocol }) =
      ctl_ipc::read_frame::<_, ClientMessage>(server)
        .await
        .unwrap()
    else {
      panic!("expected broker handshake");
    };
    assert!(protocol.accepts(ctl_ipc::PROTOCOL_VERSION));
    ctl_ipc::write_frame(
      server,
      &ServerMessage::HandshakeAccepted {
        protocol_version: ctl_ipc::PROTOCOL_VERSION,
      },
    )
    .await
    .unwrap();
    match ctl_ipc::read_frame::<_, ClientMessage>(server)
      .await
      .unwrap()
    {
      Some(ClientMessage::EnsureMaster { target }) if interactive => {
        assert_eq!(target, fixture_target());
      }
      Some(ClientMessage::EnsureMasterQuiet { target }) if !interactive => {
        assert_eq!(target, fixture_target());
      }
      _ => panic!("expected master request matching interaction policy"),
    }
  }

  async fn send_prompt(
    server: &mut tokio::io::DuplexStream,
    kind: PromptKind,
  ) -> Option<Zeroizing<String>> {
    ctl_ipc::write_frame(
      server,
      &ServerMessage::Prompt {
        prompt_id: "fixture-prompt".into(),
        kind,
        message: "fixture message".into(),
        warning: Some("fixture warning".into()),
      },
    )
    .await
    .unwrap();
    match ctl_ipc::read_frame::<_, ClientMessage>(server)
      .await
      .unwrap()
    {
      Some(ClientMessage::PromptResponse {
        prompt_id,
        response,
      }) if prompt_id == "fixture-prompt" => response,
      _ => panic!("expected matching prompt response"),
    }
  }

  #[tokio::test]
  async fn headless_retries_cancel_authentication_without_starting_a_terminal_reader() {
    let mut calls = 0;
    for _ in 0..5 {
      for kind in [PromptKind::Secret, PromptKind::Confirm] {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let daemon = tokio::spawn(async move {
          accept_master_request(&mut server, false).await;
          assert!(send_prompt(&mut server, kind).await.is_none());
        });
        let result = tokio::time::timeout(
          std::time::Duration::from_secs(1),
          ensure_master_on(&mut client, fixture_target(), false, |_, _, _| {
            calls += 1;
            std::future::ready(Ok(None))
          }),
        )
        .await
        .expect("headless authentication must finish without waiting for terminal input");
        assert!(matches!(result, Err(Error::AuthenticationRequired)));
        daemon.await.unwrap();
      }
    }
    assert_eq!(
      calls, 0,
      "headless retries must never invoke terminal prompts"
    );
  }

  #[tokio::test]
  async fn headless_credential_notifications_preserve_a_successful_master() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      accept_master_request(&mut server, false).await;
      assert_eq!(
        send_prompt(&mut server, PromptKind::CredentialSave)
          .await
          .as_deref()
          .map(String::as_str),
        Some("no")
      );
      assert_eq!(
        send_prompt(&mut server, PromptKind::CredentialSaveError)
          .await
          .as_deref()
          .map(String::as_str),
        Some("confirm")
      );
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::MasterReady {
          control_path: PathBuf::from("/tmp/fixture-control"),
        },
      )
      .await
      .unwrap();
    });
    let result = ensure_master_on(&mut client, fixture_target(), false, |_, _, _| {
      std::future::ready(Err(Error::PromptWorkerStopped))
    })
    .await
    .unwrap();
    assert_eq!(result, PathBuf::from("/tmp/fixture-control"));
    daemon.await.unwrap();
  }

  #[tokio::test]
  async fn interactive_master_requests_still_forward_the_prompt_and_warning() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      accept_master_request(&mut server, true).await;
      assert_eq!(
        send_prompt(&mut server, PromptKind::Confirm)
          .await
          .as_deref()
          .map(String::as_str),
        Some("yes")
      );
      ctl_ipc::write_frame(
        &mut server,
        &ServerMessage::MasterReady {
          control_path: PathBuf::from("/tmp/fixture-control"),
        },
      )
      .await
      .unwrap();
    });
    let result = ensure_master_on(
      &mut client,
      fixture_target(),
      true,
      |kind, message, warning| {
        assert_eq!(kind, PromptKind::Confirm);
        assert_eq!(message, "fixture message");
        assert_eq!(warning.as_deref(), Some("fixture warning"));
        std::future::ready(Ok(Some(Zeroizing::new("yes".into()))))
      },
    )
    .await
    .unwrap();
    assert_eq!(result, PathBuf::from("/tmp/fixture-control"));
    daemon.await.unwrap();
  }

  #[tokio::test]
  async fn older_brokers_only_receive_passive_status_during_background_reconnect() {
    for selected in [ctl_ipc::CONTRACT_V1_0_12, ctl_ipc::CONTRACT_V1_1_13] {
      for ready in [false, true] {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let daemon = tokio::spawn(async move {
          assert!(matches!(
            ctl_ipc::read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
            Some(ClientMessage::Handshake { protocol }) if protocol.accepts(selected)
          ));
          ctl_ipc::write_frame(
            &mut server,
            &ServerMessage::HandshakeAccepted {
              protocol_version: selected,
            },
          )
          .await
          .unwrap();
          assert!(matches!(
            ctl_ipc::read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
            Some(ClientMessage::MasterStatus { target }) if target == fixture_target()
          ));
          let response = if ready {
            ServerMessage::MasterReady {
              control_path: PathBuf::from("/tmp/fixture-control"),
            }
          } else {
            ServerMessage::AuthenticationRequired
          };
          ctl_ipc::write_frame(&mut server, &response).await.unwrap();
        });
        let result = ensure_master_on(&mut client, fixture_target(), false, |_, _, _| {
          std::future::ready(Err(Error::PromptWorkerStopped))
        })
        .await;
        if ready {
          assert_eq!(result.unwrap(), PathBuf::from("/tmp/fixture-control"));
        } else {
          assert!(matches!(result, Err(Error::AuthenticationRequired)));
        }
        daemon.await.unwrap();
      }
    }
  }

  #[tokio::test]
  async fn quiet_broker_can_require_approval_without_sending_a_prompt() {
    let (mut client, mut server) = tokio::io::duplex(4096);
    let daemon = tokio::spawn(async move {
      accept_master_request(&mut server, false).await;
      ctl_ipc::write_frame(&mut server, &ServerMessage::AuthenticationRequired)
        .await
        .unwrap();
    });
    let result = ensure_master_on(&mut client, fixture_target(), false, |_, _, _| {
      std::future::ready(Err(Error::PromptWorkerStopped))
    })
    .await;
    assert!(matches!(result, Err(Error::AuthenticationRequired)));
    daemon.await.unwrap();
  }

  #[test]
  fn explicit_quiet_requests_require_the_selected_contract() {
    let request = ClientMessage::EnsureMasterQuiet {
      target: fixture_target(),
    };
    for selected in [ctl_ipc::CONTRACT_V1_0_12, ctl_ipc::CONTRACT_V1_1_13] {
      assert!(matches!(
        validate_request_contract(&request, selected),
        Err(Error::UnsupportedQuietMaster(actual)) if actual == selected
      ));
    }
    validate_request_contract(&request, ctl_ipc::CONTRACT_V1_1_14).unwrap();
  }

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
    for selected in ctl_ipc::SUPPORTED_PROTOCOL_VERSIONS.iter().copied() {
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
