use super::{client_identity, unexpected_response};
use crate::{
  dto::{ConnectionTargetDto, TerminalCheckpointDto, TerminalHistorySnapshotDto},
  error::{CommandErrorDto, CommandResult},
  transport,
};
use rmux_proto::{ArchivedTerminalInfo, ClientMessage, ServerMessage, ViewLayout};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ArchiveRequest {
  target: ConnectionTargetDto,
  action: ArchiveAction,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveAction {
  List,
  Read {
    session_id: String,
    terminal_id: String,
  },
}

#[derive(Serialize)]
pub struct ArchiveSummary {
  session_id: String,
  name: String,
  created_at_ms: u64,
  archived_at_ms: u64,
  expires_at_ms: u64,
  layout: ViewLayout,
  terminals: Vec<ArchivedTerminalInfo>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveResponse {
  List {
    archives: Vec<ArchiveSummary>,
  },
  Terminal {
    info: ArchivedTerminalInfo,
    checkpoint: TerminalCheckpointDto,
    history: Box<TerminalHistorySnapshotDto>,
  },
}

#[tauri::command]
pub async fn session_archive(request: ArchiveRequest) -> CommandResult<ArchiveResponse> {
  tokio::time::timeout(std::time::Duration::from_secs(30), async {
    let message = match request.action {
      ArchiveAction::List => ClientMessage::ListArchives,
      ArchiveAction::Read {
        session_id,
        terminal_id,
      } => ClientMessage::GetArchivedTerminal {
        session_id,
        terminal_id,
      },
    };
    let stream = transport::connect(&request.target).await?;
    match rmux_client::request(stream, &client_identity(), message)
      .await
      .map_err(CommandErrorDto::client)?
    {
      ServerMessage::ArchiveList { archives } => Ok(ArchiveResponse::List {
        archives: archives
          .into_iter()
          .map(|archive| ArchiveSummary {
            session_id: archive.session.session_id,
            name: archive.session.name,
            created_at_ms: archive.session.created_at_ms,
            archived_at_ms: archive.archived_at_ms,
            expires_at_ms: archive.expires_at_ms,
            layout: archive.view.layout,
            terminals: archive.terminals,
          })
          .collect(),
      }),
      ServerMessage::ArchivedTerminalSnapshot { terminal } => Ok(ArchiveResponse::Terminal {
        info: terminal.info,
        checkpoint: terminal.checkpoint.into(),
        history: Box::new(terminal.history.into()),
      }),
      response => Err(unexpected_response("archive", &response)),
    }
  })
  .await
  .map_err(|_| CommandErrorDto::new("archive_timeout", "Archive request timed out."))?
}
