use super::{client_identity, unexpected_response};
use crate::dto::{ConnectionTargetDto, TerminalSizeDto};
use crate::error::{CommandErrorDto, CommandResult};
use crate::transport;
use rmux_proto::{ClientMessage, ServerMessage, SplitAxis, ViewLayout};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ViewRequest {
  target: ConnectionTargetDto,
  action: ViewAction,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewAction {
  Get {
    session_id: String,
  },
  Split {
    terminal_id: String,
    axis: SplitAxis,
    terminal_size: TerminalSizeDto,
    working_directory: Option<String>,
  },
  Update {
    session_id: String,
    expected_revision: String,
    layout: ViewLayout,
  },
  Promote {
    terminal_id: String,
    name: Option<String>,
  },
  Merge {
    source: String,
    destination: String,
  },
  KillTerminal {
    terminal_id: String,
  },
}

#[derive(Serialize)]
pub struct ViewDto {
  session_name: String,
  view_id: String,
  session_id: String,
  revision: String,
  layout: ViewLayout,
  terminals: Vec<TerminalDto>,
}

#[derive(Serialize)]
pub struct TerminalDto {
  terminal_id: String,
  name: String,
  next_sequence: String,
  terminal_size: TerminalSizeDto,
}

#[tauri::command]
pub async fn session_view(request: ViewRequest) -> CommandResult<Option<ViewDto>> {
  let message = match request.action {
    ViewAction::Get { session_id } => ClientMessage::GetView {
      session: session_id,
    },
    ViewAction::Split {
      terminal_id,
      axis,
      terminal_size,
      working_directory,
    } => ClientMessage::SplitTerminal {
      terminal_id,
      axis,
      command: None,
      working_directory,
      terminal_size: terminal_size.into_proto()?,
    },
    ViewAction::Update {
      session_id,
      expected_revision,
      layout,
    } => ClientMessage::UpdateView {
      session: session_id,
      expected_revision: expected_revision
        .parse()
        .map_err(CommandErrorDto::backend)?,
      layout,
    },
    ViewAction::Promote { terminal_id, name } => {
      ClientMessage::PromoteTerminal { terminal_id, name }
    }
    ViewAction::Merge {
      source,
      destination,
    } => ClientMessage::MergeSessions {
      source,
      destination,
    },
    ViewAction::KillTerminal { terminal_id } => ClientMessage::KillTerminal { terminal_id },
  };
  let stream = transport::connect(&request.target).await?;
  match rmux_client::request(stream, &client_identity(), message)
    .await
    .map_err(CommandErrorDto::client)?
  {
    ServerMessage::ViewSnapshot { view } => Ok(Some(ViewDto {
      session_name: view.session_name,
      view_id: view.view_id,
      session_id: view.session_id,
      revision: view.revision.to_string(),
      layout: view.layout,
      terminals: view
        .terminals
        .into_iter()
        .map(|terminal| TerminalDto {
          terminal_id: terminal.terminal_id,
          name: terminal.name,
          next_sequence: terminal.next_sequence.to_string(),
          terminal_size: terminal.terminal_size.into(),
        })
        .collect(),
    })),
    ServerMessage::Success => Ok(None),
    response => Err(unexpected_response("view_snapshot", &response)),
  }
}
