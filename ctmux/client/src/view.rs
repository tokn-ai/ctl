//! Typed one-shot view and terminal actions over an explicitly selected transport.

use crate::{ClientError, ClientIdentity, request, session::SessionId, unexpected};
use ctmux_proto::{
  ClientMessage, CommandSpec, ServerMessage, SplitAxis, TerminalSize, ViewInfo, ViewLayout,
};
use tokio::io::{AsyncRead, AsyncWrite};

/// A terminal selector resolved by the calling adapter in this target's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalId(pub String);

/// Parameters for creating a terminal beside an existing terminal.
#[derive(Debug, Clone)]
pub struct SplitTerminalRequest {
  pub terminal_id: TerminalId,
  pub axis: SplitAxis,
  pub command: Vec<String>,
  pub cwd: Option<String>,
  pub terminal_size: TerminalSize,
}

/// A layout replacement guarded by the revision observed by the caller.
#[derive(Debug, Clone)]
pub struct UpdateViewRequest {
  pub session_id: SessionId,
  pub expected_revision: u64,
  pub layout: ViewLayout,
}

/// One typed exchange on an already selected target's stream.
///
/// Consuming this client closes the one-shot transport after its response.
/// Target resolution and authentication belong to the caller's connector;
/// mutations are never retried or replayed after transport loss.
pub struct ViewClient<S> {
  stream: S,
  identity: ClientIdentity,
}

impl<S: AsyncRead + AsyncWrite + Unpin> ViewClient<S> {
  #[must_use]
  pub fn new(stream: S, identity: ClientIdentity) -> Self {
    Self { stream, identity }
  }

  /// Inspect a session's current view without attaching.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors.
  pub async fn get(self, session_id: SessionId) -> Result<ViewInfo, ClientError> {
    self
      .view_request(ClientMessage::GetView {
        session: session_id.0,
      })
      .await
  }

  /// Create a terminal beside the selected terminal and return its view.
  /// An empty command selects the daemon's default shell.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn split(self, parameters: SplitTerminalRequest) -> Result<ViewInfo, ClientError> {
    let mut argv = parameters.command.into_iter();
    let command = argv.next().map(|program| CommandSpec {
      program,
      arguments: argv.collect(),
    });
    self
      .view_request(ClientMessage::SplitTerminal {
        terminal_id: parameters.terminal_id.0,
        axis: parameters.axis,
        command,
        working_directory: parameters.cwd,
        terminal_size: parameters.terminal_size,
      })
      .await
  }

  /// Replace a view's layout only if its revision still matches.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn update(self, parameters: UpdateViewRequest) -> Result<ViewInfo, ClientError> {
    self
      .view_request(ClientMessage::UpdateView {
        session: parameters.session_id.0,
        expected_revision: parameters.expected_revision,
        layout: parameters.layout,
      })
      .await
  }

  /// Move a terminal into a new session and return the new view.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn promote(
    self,
    terminal_id: TerminalId,
    name: Option<String>,
  ) -> Result<ViewInfo, ClientError> {
    self
      .view_request(ClientMessage::PromoteTerminal {
        terminal_id: terminal_id.0,
        name,
      })
      .await
  }

  /// Move all terminals from the source session into the destination view.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn merge(
    self,
    source: SessionId,
    destination: SessionId,
  ) -> Result<ViewInfo, ClientError> {
    self
      .view_request(ClientMessage::MergeSessions {
        source: source.0,
        destination: destination.0,
      })
      .await
  }

  /// Terminate a terminal, not merely this client's attachment.
  /// # Errors
  /// Returns handshake, transport, or daemon response errors; never retries.
  pub async fn terminate_terminal(self, terminal_id: TerminalId) -> Result<(), ClientError> {
    match request(
      self.stream,
      &self.identity,
      ClientMessage::KillTerminal {
        terminal_id: terminal_id.0,
      },
    )
    .await?
    {
      ServerMessage::Success => Ok(()),
      response => Err(unexpected("success", &response)),
    }
  }

  async fn view_request(self, message: ClientMessage) -> Result<ViewInfo, ClientError> {
    match request(self.stream, &self.identity, message).await? {
      ServerMessage::ViewSnapshot { view } => Ok(view),
      response => Err(unexpected("view_snapshot", &response)),
    }
  }
}
