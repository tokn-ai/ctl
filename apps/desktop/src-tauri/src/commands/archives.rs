use crate::error::{CommandErrorDto, CommandResult};
use rmux_client::archive::{ArchiveStore, SessionArchive};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ArchiveRequest {
  action: ArchiveAction,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveAction {
  List,
  Save { archive: SessionArchive },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveResponse {
  List { archives: Vec<SessionArchive> },
  Saved,
}

#[tauri::command]
pub async fn session_archive(request: ArchiveRequest) -> CommandResult<ArchiveResponse> {
  tokio::task::spawn_blocking(move || {
    let store = ArchiveStore::for_client("desktop")?;
    match request.action {
      ArchiveAction::List => Ok(ArchiveResponse::List {
        archives: store.list()?,
      }),
      ArchiveAction::Save { archive } => {
        store.save(archive)?;
        Ok(ArchiveResponse::Saved)
      }
    }
  })
  .await
  .map_err(CommandErrorDto::backend)?
  .map_err(|error: std::io::Error| CommandErrorDto::backend(error))
}
