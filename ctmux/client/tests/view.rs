use ctmux_client::{
  ClientError, ClientIdentity,
  session::SessionId,
  view::{SplitTerminalRequest, TerminalId, UpdateViewRequest, ViewClient},
};
use ctmux_proto::{
  ClientMessage, CommandSpec, ErrorCode, ServerMessage, SplitAxis, TerminalSize, ViewInfo,
  ViewLayout, read_frame, write_frame,
};
use tokio::io::{AsyncWriteExt, DuplexStream};

#[derive(Clone, Copy, Debug)]
enum Action {
  Get,
  Split,
  Update,
  Promote,
  Merge,
  Terminate,
}

const ACTIONS: [Action; 6] = [
  Action::Get,
  Action::Split,
  Action::Update,
  Action::Promote,
  Action::Merge,
  Action::Terminate,
];

fn identity() -> ClientIdentity {
  ClientIdentity {
    name: "selected-transport-test".into(),
    version: "test-version".into(),
  }
}

fn geometry() -> TerminalSize {
  TerminalSize {
    columns: 132,
    rows: 37,
    pixel_width: 1056,
    pixel_height: 592,
  }
}

fn layout() -> ViewLayout {
  ViewLayout::Split {
    axis: SplitAxis::Vertical,
    children: vec![
      ViewLayout::Terminal {
        terminal_id: "terminal-source".into(),
      },
      ViewLayout::Terminal {
        terminal_id: "terminal-other".into(),
      },
    ],
    weights: vec![3, 7],
  }
}

fn view() -> ViewInfo {
  ViewInfo {
    session_name: "work".into(),
    view_id: "returned-view".into(),
    session_id: "returned-session".into(),
    revision: 42,
    canvas_size: geometry(),
    panes: Vec::new(),
    zoomed_terminal_id: None,
    layout: layout(),
    terminals: Vec::new(),
  }
}

fn expected(action: Action) -> ClientMessage {
  match action {
    Action::Get => ClientMessage::GetView {
      session: "session-source".into(),
    },
    Action::Split => ClientMessage::SplitTerminal {
      terminal_id: "terminal-source".into(),
      axis: SplitAxis::Horizontal,
      command: Some(CommandSpec {
        program: "printf".into(),
        arguments: vec!["hello world".into(), "$(literal)".into()],
      }),
      working_directory: Some("/work space".into()),
      terminal_size: geometry(),
    },
    Action::Update => ClientMessage::UpdateView {
      session: "session-source".into(),
      expected_revision: u64::MAX,
      layout: layout(),
    },
    Action::Promote => ClientMessage::PromoteTerminal {
      terminal_id: "terminal-source".into(),
      name: Some("new session".into()),
    },
    Action::Merge => ClientMessage::MergeSessions {
      source: "session-source".into(),
      destination: "session-destination".into(),
    },
    Action::Terminate => ClientMessage::KillTerminal {
      terminal_id: "terminal-source".into(),
    },
  }
}

async fn invoke(action: Action, stream: DuplexStream) -> Result<Option<ViewInfo>, ClientError> {
  let client = ViewClient::new(stream, identity());
  let session_id = SessionId("session-source".into());
  let terminal_id = TerminalId("terminal-source".into());
  let result = match action {
    Action::Get => client.get(session_id).await,
    Action::Split => {
      client
        .split(SplitTerminalRequest {
          terminal_id,
          axis: SplitAxis::Horizontal,
          command: vec!["printf".into(), "hello world".into(), "$(literal)".into()],
          cwd: Some("/work space".into()),
          terminal_size: geometry(),
        })
        .await
    }
    Action::Update => {
      client
        .update(UpdateViewRequest {
          session_id,
          expected_revision: u64::MAX,
          layout: layout(),
        })
        .await
    }
    Action::Promote => {
      client
        .promote(terminal_id, Some("new session".into()))
        .await
    }
    Action::Merge => {
      client
        .merge(session_id, SessionId("session-destination".into()))
        .await
    }
    Action::Terminate => return client.terminate_terminal(terminal_id).await.map(|()| None),
  };
  result.map(Some)
}

async fn accept(server: &mut DuplexStream) {
  assert!(matches!(
    read_frame::<_, ClientMessage>(server).await.unwrap(),
    Some(ClientMessage::Handshake { protocol, client_name, client_version })
      if protocol == ctmux_proto::protocol_offer()
        && client_name == identity().name
        && client_version == identity().version
  ));
  write_frame(
    server,
    &ServerMessage::HandshakeAccepted {
      protocol_version: ctmux_proto::PROTOCOL_VERSION,
      protocols: vec![ctl_core::component::ProtocolInfo::new(
        "ctmux",
        ctmux_proto::PROTOCOL_VERSION.build,
        ctmux_proto::PROTOCOL_VERSION,
        &[ctmux_proto::PROTOCOL_VERSION],
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

async fn exchange(
  action: Action,
  response: Option<ServerMessage>,
) -> Result<Option<ViewInfo>, ClientError> {
  let (client, mut server) = tokio::io::duplex(4096);
  let peer = tokio::spawn(async move {
    accept(&mut server).await;
    assert_eq!(
      read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
      Some(expected(action)),
      "{action:?}"
    );
    if let Some(response) = response {
      write_frame(&mut server, &response).await.unwrap();
    } else {
      // Lose the reply while retaining the read half to detect any replay.
      server.shutdown().await.unwrap();
    }
    assert!(
      read_frame::<_, ClientMessage>(&mut server)
        .await
        .unwrap()
        .is_none(),
      "one-shot {action:?} must close without another request"
    );
  });
  let result = invoke(action, client).await;
  peer.await.unwrap();
  result
}

#[tokio::test]
async fn actions_preserve_selectors_arguments_geometry_and_revision() {
  for action in ACTIONS {
    let (response, result) = if matches!(action, Action::Terminate) {
      (ServerMessage::Success, None)
    } else {
      (ServerMessage::ViewSnapshot { view: view() }, Some(view()))
    };
    assert_eq!(exchange(action, Some(response)).await.unwrap(), result);
  }
}

#[tokio::test]
async fn each_action_requires_its_specific_reply() {
  for action in ACTIONS {
    let (response, expected) = if matches!(action, Action::Terminate) {
      (ServerMessage::ViewSnapshot { view: view() }, "success")
    } else {
      (ServerMessage::Success, "view_snapshot")
    };
    assert!(matches!(
      exchange(action, Some(response)).await,
      Err(ClientError::UnexpectedResponse { expected: actual, .. }) if actual == expected
    ));
  }
}

#[tokio::test]
async fn daemon_errors_retain_code_and_message_without_replay() {
  for action in ACTIONS {
    assert!(matches!(
      exchange(action, Some(ServerMessage::Error {
        code: ErrorCode::InvalidRequest,
        message: "view revision changed".into(),
      })).await,
      Err(ClientError::Server { code: ErrorCode::InvalidRequest, message })
        if message == "view revision changed"
    ));
  }
}

#[tokio::test]
async fn lost_mutation_reply_is_reported_without_replay() {
  assert!(matches!(
    exchange(Action::Split, None).await,
    Err(ClientError::UnexpectedEof)
  ));
}

#[tokio::test]
async fn cancelling_a_mutation_closes_its_selected_transport() {
  let (client, mut server) = tokio::io::duplex(4096);
  let mutation = tokio::spawn(invoke(Action::Update, client));
  accept(&mut server).await;
  assert_eq!(
    read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
    Some(expected(Action::Update))
  );
  mutation.abort();
  assert!(mutation.await.unwrap_err().is_cancelled());
  assert!(
    read_frame::<_, ClientMessage>(&mut server)
      .await
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn handshake_rejection_prevents_the_mutation() {
  let (client, mut server) = tokio::io::duplex(4096);
  let mutation = tokio::spawn(invoke(Action::Split, client));
  assert!(matches!(
    read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
    Some(ClientMessage::Handshake { .. })
  ));
  write_frame(
    &mut server,
    &ServerMessage::Error {
      code: ErrorCode::ProtocolVersionMismatch,
      message: "no common contract".into(),
    },
  )
  .await
  .unwrap();
  assert!(matches!(
    mutation.await.unwrap(),
    Err(ClientError::Server { code: ErrorCode::ProtocolVersionMismatch, message })
      if message == "no common contract"
  ));
  assert!(
    read_frame::<_, ClientMessage>(&mut server)
      .await
      .unwrap()
      .is_none()
  );
}

#[tokio::test]
async fn empty_split_command_selects_default_shell() {
  let (client, mut server) = tokio::io::duplex(4096);
  let peer = tokio::spawn(async move {
    accept(&mut server).await;
    assert_eq!(
      read_frame::<_, ClientMessage>(&mut server).await.unwrap(),
      Some(ClientMessage::SplitTerminal {
        terminal_id: "terminal-source".into(),
        axis: SplitAxis::Vertical,
        command: None,
        working_directory: None,
        terminal_size: geometry(),
      })
    );
    write_frame(&mut server, &ServerMessage::ViewSnapshot { view: view() })
      .await
      .unwrap();
  });
  assert_eq!(
    ViewClient::new(client, identity())
      .split(SplitTerminalRequest {
        terminal_id: TerminalId("terminal-source".into()),
        axis: SplitAxis::Vertical,
        command: Vec::new(),
        cwd: None,
        terminal_size: geometry(),
      })
      .await
      .unwrap(),
    view()
  );
  peer.await.unwrap();
}
