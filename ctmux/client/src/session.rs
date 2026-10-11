//! Typed session actions over an explicitly selected transport.

use crate::{
  AttachRequest, AttachedSession, ClientError, ClientIdentity, begin_attach, handshake,
  read_response, request, unexpected,
};
use ctmux_proto::{
  ClientMessage, CommandSpec, ServerMessage, SessionInfo, TerminalSize, write_frame,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// Creation parameters independent of CLI parsing and desktop DTOs.
#[derive(Debug, Clone)]
pub struct CreateSessionRequest {
  /// Omitted or blank names request the daemon's automatic session name.
  pub name: Option<String>,
  pub cwd: Option<String>,
  pub command: Vec<String>,
  pub terminal_size: TerminalSize,
}

/// A session selector resolved by the calling adapter in this target's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId(pub String);

/// One typed exchange on an already selected target's stream.
///
/// ctmux control requests are one-shot: consuming the client prevents reuse of
/// a completed protocol stream. The caller's connector owns target resolution
/// and authentication; this API never starts a separate client process.
pub struct SessionClient<S> {
  stream: S,
  identity: ClientIdentity,
}

impl<S: AsyncRead + AsyncWrite + Unpin> SessionClient<S> {
  #[must_use]
  pub fn new(stream: S, identity: ClientIdentity) -> Self {
    Self { stream, identity }
  }

  /// List sessions without opening an attachment.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors.
  pub async fn list(self) -> Result<Vec<SessionInfo>, ClientError> {
    match request(self.stream, &self.identity, ClientMessage::ListSessions).await? {
      ServerMessage::SessionList { sessions } => Ok(sessions),
      response => Err(unexpected("session_list", &response)),
    }
  }

  /// Rename existing persistent work without changing its identity or layout.
  /// # Errors
  /// Returns an unsupported-operation error before sending a mutation to an
  /// older daemon, or a handshake, transport, or daemon response error. Never retries.
  pub async fn rename(
    mut self,
    session_id: SessionId,
    name: String,
  ) -> Result<SessionInfo, ClientError> {
    let handshake = handshake(&mut self.stream, &self.identity).await?;
    if !ctmux_proto::supports_session_rename(handshake.protocol_version) {
      return Err(ClientError::UnsupportedOperation {
        operation: "session.rename",
        protocol_version: handshake.protocol_version,
      });
    }
    write_frame(
      &mut self.stream,
      &ClientMessage::RenameSession {
        session: session_id.0,
        name,
      },
    )
    .await?;
    match read_response(&mut self.stream).await? {
      ServerMessage::SessionRenamed { session } => Ok(session),
      response => Err(unexpected("session_renamed", &response)),
    }
  }

  /// Create persistent work without attaching a presenter.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn create(self, parameters: CreateSessionRequest) -> Result<SessionInfo, ClientError> {
    let mut argv = parameters.command.into_iter();
    let command = argv.next().map(|program| CommandSpec {
      program,
      arguments: argv.collect(),
    });
    let message = ClientMessage::CreateSession {
      name: parameters.name.filter(|name| !name.trim().is_empty()),
      command,
      working_directory: parameters.cwd,
      terminal_size: parameters.terminal_size,
    };
    match request(self.stream, &self.identity, message).await? {
      ServerMessage::SessionCreated { session } => Ok(session),
      response => Err(unexpected("session_created", &response)),
    }
  }

  /// Terminate a session, not merely this client's attachment.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn terminate(self, session_id: SessionId) -> Result<(), ClientError> {
    let message = ClientMessage::KillSession {
      session: session_id.0,
    };
    match request(self.stream, &self.identity, message).await? {
      ServerMessage::Success => Ok(()),
      response => Err(unexpected("success", &response)),
    }
  }

  /// Open an attachment and transfer its live stream to the presenter/controller.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors.
  pub async fn attach(
    self,
    parameters: AttachRequest,
  ) -> Result<(S, AttachedSession), ClientError> {
    begin_attach(self.stream, &self.identity, parameters).await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use ctmux_proto::{ErrorCode, SessionStatus, read_frame, write_frame};

  fn identity() -> ClientIdentity {
    ClientIdentity {
      name: "api-test".into(),
      version: "test".into(),
    }
  }

  fn session() -> SessionInfo {
    SessionInfo {
      view_id: "view-id".into(),
      terminal_id: "terminal-id".into(),
      session_id: "session-id".into(),
      name: "example".into(),
      status: SessionStatus::Running,
      created_at_ms: 1,
      next_sequence: 0,
      terminal_size: TerminalSize {
        columns: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
      },
    }
  }

  async fn accept(server: &mut tokio::io::DuplexStream) {
    accept_contract(server, ctmux_proto::PROTOCOL_VERSION).await;
  }

  async fn accept_contract(
    server: &mut tokio::io::DuplexStream,
    version: ctl_core::protocol::ProtocolVersion,
  ) {
    assert!(
      matches!(read_frame::<_, ClientMessage>(server).await.unwrap(), Some(ClientMessage::Handshake { client_name, .. }) if client_name == "api-test")
    );
    write_frame(
      server,
      &ServerMessage::HandshakeAccepted {
        protocol_version: version,
        protocols: vec![ctl_core::component::ProtocolInfo::new(
          "ctmux",
          version.build,
          version,
          &[version],
        )],
        server_version: "test".into(),
        build: None,
        heartbeat_interval_ms: 1_000,
        attachment_liveness_timeout_ms: 3_000,
      },
    )
    .await
    .unwrap();
  }

  #[tokio::test]
  async fn rename_pins_id_and_returns_authoritative_metadata() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(ClientMessage::RenameSession { session, name })
          if session == "selected-id" && name == "renamed"
      ));
      let mut renamed = session();
      renamed.session_id = "selected-id".into();
      renamed.name = "renamed".into();
      write_frame(
        &mut server,
        &ServerMessage::SessionRenamed { session: renamed },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    let renamed = SessionClient::new(client, identity())
      .rename(SessionId("selected-id".into()), "renamed".into())
      .await
      .unwrap();
    assert_eq!(renamed.session_id, "selected-id");
    assert_eq!(renamed.name, "renamed");
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn rename_refuses_every_older_contract_without_sending_mutation() {
    for version in ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS
      .iter()
      .copied()
      .filter(|version| !ctmux_proto::supports_session_rename(*version))
    {
      let (client, mut server) = tokio::io::duplex(4096);
      let peer = tokio::spawn(async move {
        accept_contract(&mut server, version).await;
        assert!(
          read_frame::<_, ClientMessage>(&mut server)
            .await
            .unwrap()
            .is_none()
        );
      });
      assert!(matches!(
        SessionClient::new(client, identity())
          .rename(SessionId("selected-id".into()), "renamed".into())
          .await,
        Err(ClientError::UnsupportedOperation { operation: "session.rename", protocol_version })
          if protocol_version == version
      ));
      peer.await.unwrap();
    }
  }

  #[tokio::test]
  async fn rename_preserves_domain_errors_without_retry() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(ClientMessage::RenameSession { .. })
      ));
      write_frame(
        &mut server,
        &ServerMessage::Error {
          code: ErrorCode::SessionAlreadyExists,
          message: "taken".into(),
        },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    assert!(matches!(
      SessionClient::new(client, identity())
        .rename(SessionId("selected-id".into()), "taken".into())
        .await,
      Err(ClientError::Server {
        code: ErrorCode::SessionAlreadyExists,
        ..
      })
    ));
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn create_preserves_argv_cwd_and_geometry_without_attaching() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      let Some(ClientMessage::CreateSession {
        name,
        command: Some(command),
        working_directory,
        terminal_size,
      }) = read_frame(&mut server).await.unwrap()
      else {
        panic!("expected create")
      };
      assert_eq!(name.as_deref(), Some("example"));
      assert_eq!(command.program, "printf");
      assert_eq!(command.arguments, ["hello world", "$(literal)"]);
      assert_eq!(working_directory.as_deref(), Some("/work space"));
      assert_eq!(terminal_size, session().terminal_size);
      write_frame(
        &mut server,
        &ServerMessage::SessionCreated { session: session() },
      )
      .await
      .unwrap();
      // Completion must close this one-shot channel, without an attach request.
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    let result = SessionClient::new(client, identity())
      .create(CreateSessionRequest {
        name: Some("example".into()),
        cwd: Some("/work space".into()),
        command: vec!["printf".into(), "hello world".into(), "$(literal)".into()],
        terminal_size: session().terminal_size,
      })
      .await
      .unwrap();
    assert_eq!(result.session_id, "session-id");
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn create_blank_names_request_automatic_names_on_existing_contracts() {
    for version in [
      ctmux_proto::SUPPORTED_PROTOCOL_VERSIONS[0],
      ctmux_proto::PROTOCOL_VERSION,
    ] {
      for name in [None, Some(""), Some(" \t\n"), Some("\u{2003}")] {
        let (client, mut server) = tokio::io::duplex(4096);
        let peer = tokio::spawn(async move {
          accept_contract(&mut server, version).await;
          assert!(matches!(
            read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
            Some(ClientMessage::CreateSession { name: None, .. })
          ));
          let mut created = session();
          created.name = "session-42".into();
          write_frame(
            &mut server,
            &ServerMessage::SessionCreated { session: created },
          )
          .await
          .unwrap();
          assert!(
            read_frame::<_, ClientMessage>(&mut server)
              .await
              .unwrap()
              .is_none()
          );
        });
        let created = SessionClient::new(client, identity())
          .create(CreateSessionRequest {
            name: name.map(str::to_owned),
            cwd: None,
            command: Vec::new(),
            terminal_size: session().terminal_size,
          })
          .await
          .unwrap();
        assert_eq!(created.name, "session-42");
        peer.await.unwrap();
      }
    }
  }

  #[tokio::test]
  async fn create_nonblank_names_remain_literal_and_keep_domain_errors() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(ClientMessage::CreateSession { name: Some(name), .. })
          if name == " example "
      ));
      write_frame(
        &mut server,
        &ServerMessage::Error {
          code: ErrorCode::InvalidSessionName,
          message: "invalid name".into(),
        },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    assert!(matches!(
      SessionClient::new(client, identity())
        .create(CreateSessionRequest {
          name: Some(" example ".into()),
          cwd: None,
          command: Vec::new(),
          terminal_size: session().terminal_size,
        })
        .await,
      Err(ClientError::Server {
        code: ErrorCode::InvalidSessionName,
        ..
      })
    ));
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn terminate_sends_only_selected_session_and_preserves_domain_errors() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      assert!(
        matches!(read_frame::<_, ClientMessage>(&mut server).await.unwrap(), Some(ClientMessage::KillSession { session }) if session == "selected-id")
      );
      write_frame(
        &mut server,
        &ServerMessage::Error {
          code: ErrorCode::SessionNotFound,
          message: "missing".into(),
        },
      )
      .await
      .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    assert!(matches!(
      SessionClient::new(client, identity())
        .terminate(SessionId("selected-id".into()))
        .await,
      Err(ClientError::Server {
        code: ErrorCode::SessionNotFound,
        ..
      })
    ));
    peer.await.unwrap();
  }

  #[tokio::test]
  async fn list_rejects_unexpected_success_without_a_second_request() {
    let (client, mut server) = tokio::io::duplex(4096);
    let peer = tokio::spawn(async move {
      accept(&mut server).await;
      assert!(matches!(
        read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
        Some(ClientMessage::ListSessions)
      ));
      write_frame(&mut server, &ServerMessage::Success)
        .await
        .unwrap();
      assert!(
        read_frame::<_, ClientMessage>(&mut server)
          .await
          .unwrap()
          .is_none()
      );
    });
    assert!(matches!(
      SessionClient::new(client, identity()).list().await,
      Err(ClientError::UnexpectedResponse {
        expected: "session_list",
        ..
      })
    ));
    peer.await.unwrap();
  }
}
